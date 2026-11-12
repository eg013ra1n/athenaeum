//! Stage 5, register (`stage_register`): the registration passes and the registered-frame artifacts.

use super::*;

/// Planes of ONE channel (ref geometry `w × h`) resident while a register
/// fan-out worker registers a MONO frame (perf tier A Task 6a fix round 1,
/// ruling R-TA-5): the pre-existing detect+align working set, unchanged.
///
/// Measured 2026-09-19 with `/usr/bin/time -l` on `register_probe --write`
/// (a real, committed flag — see its own doc), over 3 runs against two real
/// LDN1272-test calibrated frames (mono 6224×4168):
///
/// | | detect+align+write |
/// | ---- | ---- |
/// | mono, avg of 3 | 274.6 MB ≈ 2.65 planes |
///
/// Re-measured after fix round 2's plane-at-a-time write (ruling R-TA-6):
/// for a MONO (1-channel) subject the old all-at-once writer and the new
/// streaming one hold the exact same ONE plane's worth of output buffer
/// either way, so no drop was expected here — and none of consequence
/// showed up: 2.65 planes is close to the OLD design's own detect+align-
/// ONLY baseline (2.66, fix round 1's measurement), i.e. the write's
/// marginal cost over detect+align's own peak is now negligible for mono
/// too (detect+align's peak already exceeds the one-plane write cost by
/// itself). Left at 4 — comfortably above the fresh measurement, so mono
/// admission is UNCHANGED by this fix round.
pub(crate) const REGISTER_PLANES_RESIDENT: u64 = 4;

/// OSC (3-channel) register+write planes — see [`REGISTER_PLANES_RESIDENT`]'s
/// own doc for the measurement method; same date, same probe, against two
/// real LDN1272-test debayered OSC frames (6248×4176, 3 planes).
///
/// **Fix round 1** (all-at-once writer, `all = vec![NAN; plane_len *
/// channels]`, one buffer holding EVERY channel's output together for the
/// whole write), 2 runs: detect+align only 588.2 MB ≈ 5.64 planes,
/// detect+align+write 906.7 MB ≈ 8.69 planes — the write's own delta
/// (≈ 3.05 planes) was ONE plane PER CHANNEL, resident together, which is
/// what made the OSC register admission the fan-out's tightest budget.
///
/// **Fix round 2** (ruling R-TA-6, required item — plane-at-a-time write:
/// warp plane → write plane → drop its buffer → next plane, via
/// `fits_writer::write_fits_f32_streaming_with`), 3 runs: detect+align+write
/// 592.3 MB (range 588.2–598.1 MB) ≈ 5.68 planes (range 5.64–5.73) — the
/// write's own marginal cost is now negligible, same reasoning as the mono
/// re-measurement above: only ONE plane's buffer is ever resident during
/// the write, and detect+align's own peak (5.64 planes, fix round 1's own
/// baseline) already exceeds it, so the write no longer sets the peak at
/// all — close to the senior review's own rough guess of "≈5 planes
/// total" for this constant.
/// The measured total, rounded up with the same margin fix round 1's own 9
/// carried (≈0.3 planes of headroom above the average): **6**, down from 9.
///
/// If a real run's admission ever needs re-deriving from RAM instead of
/// this measurement, `total_ram_bytes() / 4 / (6 · w · h · 4)` is the
/// worker count an OSC group's register stage gets — on this dev Mac (16
/// GiB RAM, 10 cores) at LDN1272-test's 6248×4176 OSC geometry that is
/// `admission(626_199_552, 10) = 6` (up from `admission(939_299_328, 10) =
/// 4` at the old constant of 9 — both computed against this same machine's
/// real `total_ram_bytes()`, not the illustrative "16 GB machine" figure
/// fix round 1's own doc cited, which this fix round did not attempt to
/// reproduce exactly); see the fix round's own report for the full
/// before/after admission line.
pub(crate) const REGISTER_PLANES_RESIDENT_OSC: u64 = 6;

/// Register's fan-out working set (perf tier A Task 6a fix round 1) —
/// [`REGISTER_PLANES_RESIDENT`] or [`REGISTER_PLANES_RESIDENT_OSC`] planes
/// of one reference-geometry channel, in bytes, depending on the GROUP'S
/// OWN MEASURED PLANE COUNT (fix round 2, ruling R-TA-6 M3) — the caller's
/// `PendingRegistration`/`ArtifactRewriteItem::planes`, stage 3's own
/// `MeasuredFrame::planes`, not `group.color_mode`. Keying on color mode
/// charged 9 planes for an OSC group whose debayering is off and therefore
/// produces 1-plane frames (`calibration.debayer_osc = false`) — the
/// subject the writer actually opens never carries 3 channels in that
/// case, so the working set it needs never does either. `frame_planes < 3`
/// (i.e. 1) takes the mono constant, `>= 3` takes the OSC one; there is no
/// 2-channel case in this pipeline.
pub(crate) fn register_working_set_bytes(w: u64, h: u64, frame_planes: usize) -> u64 {
    let planes = if frame_planes >= 3 {
        REGISTER_PLANES_RESIDENT_OSC
    } else {
        REGISTER_PLANES_RESIDENT
    };
    planes * w * h * 4
}

/// A group's currently-included frame count — the viability gate (≥ 3) and
/// the progress total both read it, in three places since M4a Task 4.
pub(super) fn included_count_of(rc: &RunContext, group_key: &str) -> usize {
    rc.measured
        .get(group_key)
        .map(|v| v.iter().filter(|e| e.included).count())
        .unwrap_or(0)
}

/// One frame's outcome from a single DRY [`register_group_pass`] over one
/// group: the entry index into that group's `RunContext::measured` vector,
/// the frame id, and either the alignment's `(rotation_deg, translation)`
/// or the failure text.
///
/// Only a `persist = false` pass produces these; a persisting pass returns
/// an EMPTY vector, because it records each frame's outcome where the rest
/// of the run reads it (`registration_results` and
/// `MeasuredFrame::registration`) and nothing wants a second copy. Pinned
/// by `a_dry_register_pass_writes_no_rows_and_no_outcomes`.
#[derive(Debug)]
pub(super) struct PassResult {
    pub(super) idx: usize,
    pub(super) frame_id: i64,
    pub(super) outcome: Result<(f64, (f64, f64)), String>,
}

/// One frame [`register_group_pass`] has decided to register afresh (no
/// fresh cached row), with everything resolved for it on the run thread
/// before the fan-out starts.
struct PendingRegistration {
    /// Index into the group's `RunContext::measured` vector.
    idx: usize,
    frame: GroupFrame,
    path: PathBuf,
    is_reference: bool,
    expected_hash: String,
    /// M4b: this frame's own scale window, optional plate-solve seed, and
    /// which of the two seeds leads (ruling R-T6-9).
    scale_gate: (f64, f64),
    hint: Option<Linear>,
    policy: SeedPolicy,
    /// Perf tier A Task 6a fix round 1 (ruling R-TA-5): this frame's own
    /// calibrated plane count, already known from stage 3
    /// (`MeasuredFrame::planes`) — threaded through so the fan-out worker
    /// that now writes the registered artifact never needs `rc` to learn
    /// whether the output filename gets the `_d` (debayered) marker.
    planes: usize,
    /// Tier C Task 3 (ruling C-6): this frame's own stage-1 (calibration)
    /// hash, already resolved above for `expected_hash` — kept so the
    /// `RegisterItem` build below can derive the `fits` artifact's own
    /// measurement hash (`measurement_hash_for`) without a second
    /// `calibration_hash_checked` call.
    calibrated_hash: String,
}

/// What one fan-out worker needs — the pixel-side half of a
/// [`PendingRegistration`]. The bookkeeping half stays on the run thread.
struct RegisterItem {
    frame_id: i64,
    /// Perf tier A Task 6a fix round 1: kept whole (not just `frame_id`) —
    /// the worker's own registered-artifact write needs it for
    /// `frame_file_stem`.
    frame: GroupFrame,
    path: PathBuf,
    is_reference: bool,
    scale_gate: (f64, f64),
    hint: Option<Linear>,
    policy: SeedPolicy,
    /// Perf tier 1 Task 9: this frame's detections, already known from the
    /// two-pass dry pass over the SAME group — `Some` skips
    /// [`detect_frame_stars`] entirely in the fan-out closure below. `None`
    /// for a genuine cache miss (every dry-pass item, and any persisting
    /// item the dry pass never saw — the reference's own identity row, or
    /// a frame the dry pass excluded).
    pre: Option<Arc<DetectedStars>>,
    /// Perf tier A Task 6a fix round 1: this item's `registration_results`
    /// config hash — the worker needs it to build the `RegistrationRecord`
    /// (`to_record`) itself now, so the write's provenance cards and the
    /// sequential loop's `upsert_registration` agree on ONE record instead
    /// of two independently-built ones.
    expected_hash: String,
    planes: usize,
    /// Tier C Task 3 (spec §7, ruling C-6): this frame's own fresh
    /// `fits.0` artifact, resolved on the run thread (`resolve_register_fits`)
    /// only when `pre.is_none()` (a `pre` hit skips `detect_frame_stars`
    /// entirely, so reading the artifact for it would be wasted I/O) —
    /// `None` for an OSC frame, a stale/missing artifact, or a `pre` hit.
    fits: Option<Vec<psf_signal::StarFit>>,
}

/// Perf tier A Task 6a fix round 1 (ruling R-TA-5): a register fan-out
/// worker's own registered-artifact outcome for a PERSISTING item whose
/// alignment succeeded — built and (attempted to be) written right here,
/// since the warp is the CPU cost the fan-out's admission exists to
/// parallelize (~3.3 s/plane), not something to redo one frame at a time
/// on the run thread afterward. `None` (the whole `Option`, at the call
/// site) covers the dry pass and an alignment failure — neither builds a
/// record or attempts a write here; the sequential loop's existing
/// handling for both is unchanged.
struct RegisteredWrite {
    /// Already-resolved (`to_record`) — the sequential loop's
    /// `upsert_registration` uses this SAME record, never rebuilding it.
    rec: RegistrationRecord,
    /// The hash the `registered` artifact is recorded under: the
    /// registration hash, or the composed one on a grid (plan 6b-3 D2).
    registered_hash: String,
    /// `Some(out_path)` iff the write succeeded; `None` on a write failure
    /// (logged here, non-fatal — the sequential loop then records no
    /// artifact row, and every downstream reader falls back to the
    /// on-the-fly warp for this frame).
    written: Option<PathBuf>,
    /// The write's error text when `written` is `None`; folded into the
    /// group's one "registered frames could not be written" run warning.
    write_error: Option<String>,
    /// The map the registered artifact was written with: the star map, or
    /// its composition onto the grid.
    map: PixelMap,
}

/// Failed registered-frame writes of one group's pass. The registered frame
/// is a required artifact, so a failed write must not leave the run
/// silently warping on the fly: each failure keeps its per-frame `warn!`,
/// and the pass pushes ONE run warning per group (the count and the first
/// error) from [`RegisteredWriteFailures::flush`].
#[derive(Default)]
struct RegisteredWriteFailures {
    count: usize,
    first_error: Option<String>,
}

impl RegisteredWriteFailures {
    fn note(&mut self, error: String) {
        self.count += 1;
        self.first_error.get_or_insert(error);
    }

    fn flush(self, rc: &mut RunContext, group: &IntegrationGroup) {
        let Some(first) = self.first_error else {
            return;
        };
        let count = self.count;
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            count,
            error = %first,
            "registered frames could not be written"
        );
        rc.warnings.push(format!(
            "registered frames could not be written for {count} frames of {}: {first}; they are warped on the fly",
            group.key
        ));
    }
}

/// Perf tier A Task 6a fix round 1 (ruling R-TA-5): a CACHED registration
/// (`register_group_pass`'s reused branch) whose artifact turned out to be
/// missing or stale — everything the SEPARATE rewrite fan-out needs to redo
/// just the pixel work; no re-alignment, the map/record are already known.
struct ArtifactRewriteItem {
    frame_id: i64,
    calibrated: PathBuf,
    frame: GroupFrame,
    map: PixelMap,
    rec: RegistrationRecord,
    // The hash the rewritten artifact is recorded under (plan 6b-3 D2).
    registered_hash: String,
    planes: usize,
}

/// [`SeedPolicy`] as the `seed_policy` log field's value.
fn seed_policy_field(policy: SeedPolicy) -> &'static str {
    match policy {
        SeedPolicy::QuadFirst => "quad_first",
        SeedPolicy::WcsFirst => "wcs_first",
    }
}

/// Stage 5's stored-plate-solve memo (M4b Task 2): `frame_id` → its
/// `plate_solves` row, or `None` for a frame that has never been solved.
/// One lookup per frame for the whole stage, including the reference's,
/// which every other frame's seed needs again and again — and including
/// the negative answers, so an unsolved set does not re-query per frame.
pub(super) type SolveCache = HashMap<i64, Option<PlateSolveRecord>>;

/// [`SolveCache`]'s lazy fill. A read failure is a warning, not a run
/// failure: the WCS seed is an accelerator, and losing it costs the quad
/// seed's work, nothing else.
fn solve_of(
    solves: &mut SolveCache,
    conn: &rusqlite::Connection,
    origins: &FrameOrigins,
    frame_id: i64,
) -> Option<PlateSolveRecord> {
    if !solves.contains_key(&frame_id) {
        // A frame with no origin was already logged at `error!` by
        // `origin_of`; it is treated like a read error below.
        let read = origin_of(origins, frame_id)
            .map_err(anyhow::Error::from)
            .and_then(|origin| stored_solve(conn, origin, frame_id));
        let row = match read {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!(
                    frame_id,
                    error = %e,
                    "failed to read the stored plate solve; registering without a wcs seed"
                );
                None
            }
        };
        solves.insert(frame_id, row);
    }
    solves.get(&frame_id).cloned().flatten()
}

/// One frame's registration inputs (M4b Task 2, rulings R-M4b-2/3): the
/// scale window it is judged against, and the optional plate-solve seed.
///
/// The seed is built whenever BOTH this frame and the reference carry a
/// stored plate solve (ruling R-T6-9). What the frame's implied ratio
/// decides is only the ORDER: [`SeedPolicy::WcsFirst`] when the ratio says
/// a real scale step is expected (rulings R-T2-1 / R-T6-4, the
/// [`ratio_wants_seed`] tolerance), [`SeedPolicy::QuadFirst`] otherwise —
/// the M1 path, with the seed held in reserve for the frames it drops.
///
/// The tolerance decides the order rather than the window because the
/// ratio is a quotient of two MEASURED pixel scales:
/// `GroupFrame::pixel_scale_arcsec` prefers the stored plate solve, and two
/// solves of one rig disagree (typically in the fourth digit, out to 1.6 %
/// in the measured tail), so an exact comparison would put every frame of
/// an ordinary same-scale set on the WCS path — exactly the path M1–M4a's
/// pins were measured without. The gate itself stays centred on the exact
/// ratio either way: none of this changes how a frame is judged, only
/// which seed it is offered first.
///
/// A frame with no solve on either side gets no hint at all and takes the
/// M1 path with no fallback, exactly as it did before M4b.
///
/// Both passes of stage 5 go through here — the dry two-pass run and the
/// persisting one — so a frame is never measured against one gate and
/// registered against another.
fn registration_gate_and_hint(
    solves: &mut SolveCache,
    conn: &rusqlite::Connection,
    origins: &FrameOrigins,
    frame: &GroupFrame,
    reference_frame_id: i64,
    reference_scale: Option<f64>,
) -> ((f64, f64), Option<Linear>, SeedPolicy) {
    let scale_gate = scale_gate_for(frame.pixel_scale_arcsec, reference_scale);
    let ratio = scale_ratio_for(frame.pixel_scale_arcsec, reference_scale);
    let policy = if ratio_wants_seed(ratio) {
        SeedPolicy::WcsFirst
    } else {
        SeedPolicy::QuadFirst
    };
    let hint = match (
        solve_of(solves, conn, origins, frame.frame_id),
        solve_of(solves, conn, origins, reference_frame_id),
    ) {
        (Some(subject), Some(reference)) => seed_from_solves(
            &subject,
            &reference,
            (frame.width.max(0) as usize, frame.height.max(0) as usize),
        ),
        _ => None,
    };
    (scale_gate, hint, policy)
}

/// Tier C Task 3 (spec §7, ruling C-6): a mono frame's own fresh `fits.0`
/// artifact, for [`detect_frame_stars`]/[`reference_stars`] to reuse
/// instead of detecting on the frame's own pixels a second time — Measure
/// (stage 3) has already fitted this exact plane. `None` for a multi-plane
/// (OSC) frame — ruling C-6, "OSC frames keep the luminance detection" —
/// and every caller below checks `planes` before calling this at all, so
/// this only warns about a genuine mono fallback, never an OSC frame that
/// was never a candidate for reuse.
///
/// Best-effort: a DB error, a missing/stale row, an unreadable file, or a
/// cut+truncated star list below [`align::MIN_INLIERS`] (fix round 1,
/// ruling C-18, item 5) all return `None` with exactly one `warn!` naming
/// why (`run_id`/`group_key`/`frame_id`, fix round 1 item 7) — reuse is an
/// optimization, never a correctness requirement, so nothing here ever
/// fails the stage; the caller's existing detection path is always the
/// fallback.
pub(super) fn resolve_register_fits(
    rc: &RunContext,
    group_key: &str,
    frame_id: i64,
    planes: usize,
    calibrated_hash: &str,
) -> Option<Vec<psf_signal::StarFit>> {
    if planes != 1 {
        return None;
    }
    // Ruling R3 (b): the invariant is structural. Registration folds Measure's output only for a group whose frames
    // read `fits.0` (`register_reads_measure_fits`), so a group that does not read it never reaches the artifact: a
    // frame that calibrates to one plane inside a three-plane group (an unparseable BAYERPAT) detects instead, the
    // safe direction.
    let prepare_path = crate::stacking::origin::subject_prepares(&rc.subject, &rc.origins);
    let reads_fits = rc
        .plan_groups
        .iter()
        .find(|g| g.key == group_key)
        .is_some_and(|g| {
            crate::stacking::plan::register_reads_measure_fits(&rc.config, g, prepare_path)
        });
    if !reads_fits {
        return None;
    }
    let expected_hash = group_measurement_hash(rc, &rc.config, group_key, calibrated_hash);
    let kind = ArtifactKind::Fits(0);

    // Ruling C-18, item 8: the SAME per-plane freshness rule the plan gate
    // applies (`plan::fits_artifact_fresh`/`fits_artifact_fresh_for_planes`)
    // — never a hand-rolled `find_artifact` + `is_fresh` pair that could
    // quietly drift from it. `planes` is already known to be 1 here, so
    // this checks exactly the one `fits.0` row.
    let fresh = (|| -> Result<bool, RunError> {
        let conn = db(&rc.ctx)?.conn();
        Ok(fits_artifact_fresh_for_planes(
            &conn,
            &rc.subject,
            frame_id,
            planes,
            &expected_hash,
        )?)
    })();
    let fresh = match fresh {
        Ok(fresh) => fresh,
        Err(e) => {
            // `RunError` carries no `Display`, only `Debug` — the same
            // convention every other `RunError`-typed warn in the run
            // already follows (`error = ?e`).
            tracing::warn!(
                run_id = rc.run_id,
                group_key,
                frame_id,
                error = ?e,
                "register: fits artifact lookup failed; detecting"
            );
            return None;
        }
    };
    if !fresh {
        tracing::warn!(
            run_id = rc.run_id,
            group_key,
            frame_id,
            "register: no fresh fits artifact; detecting"
        );
        return None;
    }

    // The freshness check above only answers yes/no — a second, separate
    // lookup gets the row's own `path` to actually read the file.
    let row = (|| -> Result<Option<crate::db::stacking::StackingArtifactRow>, RunError> {
        let conn = db(&rc.ctx)?.conn();
        Ok(crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            group_key,
            kind,
            Some(frame_id),
        )?)
    })();
    let path = match row {
        Ok(row) => row.and_then(|r| r.path),
        Err(e) => {
            tracing::warn!(
                run_id = rc.run_id,
                group_key,
                frame_id,
                error = ?e,
                "register: fits artifact lookup failed; detecting"
            );
            return None;
        }
    };
    let Some(path) = path else {
        tracing::warn!(
            run_id = rc.run_id,
            group_key,
            frame_id,
            "register: fits artifact row has no path; detecting"
        );
        return None;
    };
    let fits = match fits_artifact::read_fits(Path::new(&path)) {
        Ok(fits) => fits,
        Err(e) => {
            tracing::warn!(
                run_id = rc.run_id,
                group_key,
                frame_id,
                path = %path,
                error = %e,
                "register: fits artifact unreadable; detecting"
            );
            return None;
        }
    };

    // Ruling C-18, item 5: a floor on the CUT+TRUNCATED list, not the raw
    // fits count — `stars_from_fits` may drop most of a frame's fits to
    // saturation/eccentricity/SNR cuts, and a star list too short to ever
    // reach `align::MIN_INLIERS` matched pairs is no better than no reuse
    // at all. This runs the SAME cut+truncate `detect_frame_stars`'s own
    // fits branch will (a cheap, allocation-only pass over an in-memory
    // `Vec`, no I/O) to judge it here rather than handing a doomed list
    // downstream and discovering the alignment failure only after RANSAC.
    let usable = stars_from_fits(
        &fits,
        &rc.config.registration.detection,
        rc.config.registration.max_stars,
    )
    .len();
    if usable < align::MIN_INLIERS {
        tracing::warn!(
            run_id = rc.run_id,
            group_key,
            frame_id,
            count = usable,
            "register: fits artifact has too few usable stars after cuts; detecting"
        );
        return None;
    }

    Some(fits)
}

/// [`resolve_register_fits`] for a frame identified only by `frame_id` —
/// the reference-detection call sites (`stage_register`,
/// `register_native_groups`) know the reference's `frame_id` before they
/// know which group it belongs to. A linear scan of `RunContext::measured`
/// (every group, every measured frame) is fine here: this runs at most
/// once per group per stage-5 pass, never per subject frame.
fn resolve_reference_fits(
    rc: &RunContext,
    frame_id: i64,
    calibrated_hash: &str,
) -> Option<Vec<psf_signal::StarFit>> {
    let (group_key, planes) = rc.measured.iter().find_map(|(key, frames)| {
        frames
            .iter()
            .find(|f| f.frame.frame_id == frame_id)
            .map(|f| (key.clone(), f.planes))
    })?;
    resolve_register_fits(rc, &group_key, frame_id, planes, calibrated_hash)
}

/// The rows the low-inlier diagnostic reads (robust quad seed RQ-6): `(frame_id, inliers, model)` of a group's
/// aligned non-reference frames, cached rows included. A failed frame, a frame with no outcome and the reference
/// are not part of the group's median.
fn low_inlier_rows(entries: &[MeasuredFrame]) -> Vec<(i64, usize, String)> {
    entries
        .iter()
        .filter_map(|e| match &e.registration {
            Some(RegisteredFrameOutcome::Aligned { record, .. }) if !record.is_reference => Some((
                e.frame.frame_id,
                record.matched_stars.max(0) as usize,
                record.model.clone().unwrap_or_default(),
            )),
            _ => None,
        })
        .collect()
}

/// Robust quad seed RQ-6: one `warn!` per frame whose registration kept under half its group's median inliers,
/// whatever seed carried it (`model` names it). The defect this counts stood through three catalog stages because
/// nothing counted it (R-T6-9's precedent: a distinct event makes the pattern countable). A log line only: no run
/// warning, no row, no UI.
fn warn_low_inlier_frames(run_id: i64, group_key: &str, rows: &[(i64, usize, String)]) {
    let counts: Vec<(i64, usize)> = rows.iter().map(|(id, n, _)| (*id, *n)).collect();
    let Some((median_inliers, flagged)) = low_inlier_frames(&counts) else {
        return;
    };
    for (frame_id, inliers) in flagged {
        let model = rows
            .iter()
            .find(|r| r.0 == frame_id)
            .map(|r| r.2.as_str())
            .unwrap_or_default();
        tracing::warn!(
            run_id,
            group_key = %group_key,
            frame_id,
            inliers,
            median_inliers,
            model = %model,
            "registration kept under half its group's median inliers"
        );
    }
}

/// Robust quad seed RQ-6, review r1 I1: the low-inlier diagnostic of a PERSISTING pass, read from the outcomes
/// already on the group's entries — every exit of [`register_group_pass`] that follows the cache walk calls it, so a
/// group whose every frame was a cache hit (nothing re-registered, an early return) is counted all the same.
fn note_low_inlier_frames(rc: &RunContext, group: &IntegrationGroup) {
    let rows = rc
        .measured
        .get(&group.key)
        .map(|entries| low_inlier_rows(entries))
        .unwrap_or_default();
    warn_low_inlier_frames(rc.run_id, &group.key, &rows);
}

/// The Register stage's progress `message` (frame-level cache spec §5.6): the two-pass dry pass.
const REGISTER_MESSAGE_CHECKING: &str = "checking the reference";
/// The Register stage's progress `message` for the persisting pass.
const REGISTER_MESSAGE_ALIGNING: &str = "aligning";

/// The reference's term of every member's registration hash (ruling R3 (a), [`reference_measurement_fold`]): its own
/// Measure output when it lives in a group other than `group`, `""` otherwise. `reference_hash` is the reference's
/// calibration hash. The one place the run, the gate and the two-pass key resolve it.
///
/// [`reference_measurement_fold`]: crate::stacking::plan::reference_measurement_fold
pub(super) fn reference_measurement_term(
    rc: &RunContext,
    group: &IntegrationGroup,
    reference_frame_id: i64,
    reference_hash: &str,
    prepare_path: bool,
) -> Result<String, RunError> {
    let reference_group = rc
        .plan_groups
        .iter()
        .find(|g| g.frames.iter().any(|f| f.frame_id == reference_frame_id))
        .ok_or_else(|| {
            RunError::Other(format!(
                "registration reference (frame {reference_frame_id}) is in no group"
            ))
        })?;
    Ok(crate::stacking::plan::reference_measurement_fold(
        &rc.config,
        group,
        reference_group,
        prepare_path,
        reference_hash,
        rc.measured_groups
            .get(&reference_group.key)
            .and_then(|g| g.beta),
    ))
}

/// One member's calibration hash and the registration hash a row for it must carry against the reference (ruling
/// R3 (d)): exactly what [`register_group_pass`] compares a stored row to, and what the two-pass decision key folds.
/// `reference_measurement_hash` is [`reference_measurement_term`]'s value.
pub(super) fn member_registration_hash(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    frame: &GroupFrame,
    reference_frame_id: i64,
    reference_hash: &str,
    reference_measurement_hash: &str,
    prepare_path: bool,
) -> Result<(String, String), RunError> {
    let cfg = rc.config.clone();
    let frame_hash = {
        let conn = db(&rc.ctx)?.conn();
        rc.memo.calibration_hash_checked(&conn, &cfg, frame)?
    };
    // Ruling C-18: the same measurement hash `plan.rs`'s `compute_register_stale` folds in - must match, or the gate
    // and the run disagree about whether a stored registration row is fresh.
    let frame_measurement_hash = crate::stacking::plan::registration_measurement_fold(
        &cfg,
        group,
        prepare_path,
        &frame_hash,
        rc.measured_groups.get(&group.key).and_then(|g| g.beta),
    );
    let expected = registration_hash_for(
        &cfg,
        reference_frame_id,
        reference_hash,
        &frame_hash,
        &frame_measurement_hash,
        reference_measurement_hash,
    );
    Ok((frame_hash, expected))
}

/// ONE registration pass over ONE group (M4a Task 4, ruling R-M4a-5 — the
/// body [`stage_register`]'s per-group loop used to inline).
///
/// `persist = true` is stage 5 as it has always been: an existing fresh
/// `registration_results` row is reused, everything else is registered and
/// then WRITTEN — the row ([`crate::stacking::plan::store_registration`]), the optional registered
/// artifact, the entry's own `registration` outcome, and the
/// exclusion/warning bookkeeping a failure implies.
///
/// `persist = false` is the two-pass pick's DRY pass. It never consults the
/// cache (pass 1 always registers afresh — a cached row belongs to the
/// FINAL reference, which is exactly what this pass exists to choose),
/// writes no row and no artifact, stores nothing on the entry, excludes
/// nobody and pushes no warning: a frame that cannot align against the
/// CURRENT reference may well align against the one this pass is about to
/// pick, so judging it here would judge it against a reference the run is
/// about to discard. Its verdicts come back in the returned [`PassResult`]s
/// instead — and `selection.exclude_on_registration_failure = false` does
/// NOT fail the run from a dry pass for the same reason.
///
/// `progress` is `(current, total)`, threaded through both passes so the
/// Register stage's progress bar counts them as one stage.
#[allow(clippy::too_many_arguments)]
pub(super) fn register_group_pass(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    ref_stars: &ReferenceStars,
    reference_frame_id: i64,
    reference_hash: &str,
    by_frame: &HashMap<i64, RegistrationRecord>,
    force_fresh: bool,
    persist: bool,
    progress: &mut (usize, usize),
    solves: &mut SolveCache,
) -> Result<Vec<PassResult>, RunError> {
    let cfg = rc.config.clone();
    // Every event of the pass names it (frame-level cache spec §5.6).
    let message = if persist {
        REGISTER_MESSAGE_ALIGNING
    } else {
        REGISTER_MESSAGE_CHECKING
    };
    let mut pass: Vec<PassResult> = Vec::new();
    let mut write_failures = RegisteredWriteFailures::default();

    if included_count_of(rc, &group.key) < 3 {
        return Ok(pass);
    }

    // Plan 6b-2 C9: the Register boundary of the landed-file identity check.
    if exclude_changed_members(rc, group)? > 0 && included_count_of(rc, &group.key) < 3 {
        return Ok(pass);
    }

    let snapshot: Vec<(usize, GroupFrame, PathBuf, bool)> = rc
        .measured
        .get(&group.key)
        .map(|v| {
            v.iter()
                .enumerate()
                .filter(|(_, e)| e.included)
                .filter_map(|(i, e)| {
                    e.calibrated.clone().map(|p| {
                        (
                            i,
                            e.frame.clone(),
                            p,
                            e.frame.frame_id == reference_frame_id,
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // The reference's OWN pixel scale, re-read here rather than passed in:
    // the two-pass pick can move the reference between this function's two
    // callers, and looking it up per pass keeps the gate honest without a
    // second parameter to keep in step.
    let reference_scale = find_frame_in_groups(&rc.plan_groups, reference_frame_id)
        .and_then(|f| f.pixel_scale_arcsec);

    let origins = rc.origins.clone();
    // Spec §5.5, plan 6b-3 D1: on a project's grid every frame this pass
    // writes is composed onto the grid through its reference's WCS. The WCS
    // rule (stage 4) guarantees the reference has one; its absence here is a
    // pipeline fault, so it fails the run rather than writing
    // reference-geometry frames.
    let compose: Option<(Arc<GridRun>, Arc<ReferenceWcs>)> = match (&rc.grid, persist) {
        (Some(grid), true) => {
            let record = {
                let conn = db(&rc.ctx)?.conn();
                solve_of(solves, &conn, &origins, reference_frame_id)
            };
            let wcs = record
                .as_ref()
                .and_then(ReferenceWcs::from_record)
                .ok_or_else(|| {
                    RunError::Other(format!(
                        "the registration reference (frame {reference_frame_id}) has no usable WCS; the grid needs one"
                    ))
                })?;
            Some((Arc::clone(grid), Arc::new(wcs)))
        }
        _ => None,
    };
    let mut to_register: Vec<PendingRegistration> = Vec::new();
    // Perf tier A Task 6a fix round 1 (ruling R-TA-5): cache-hit
    // registrations whose artifact needs (re)writing — see the `reused`
    // branch below.
    let mut needs_artifact_rewrite: Vec<ArtifactRewriteItem> = Vec::new();
    // The `registered` row each queued write replaces, read and deleted when the frame was queued (the write order,
    // file-identity spec §4.2) and carried to `record_registered_artifact`.
    let mut old_registered: HashMap<i64, crate::db::stacking::StackingArtifactRow> = HashMap::new();
    // Frame-level cache spec §5.5: the measurement hash is folded only for a frame whose registration reads
    // `fits.0`; the gate derives the same flag (`plan::compute_register_stale`).
    let prepare_path = crate::stacking::origin::subject_prepares(&rc.subject, &rc.origins);
    // Ruling R3 (a): the reference's own Measure output, when its fits seed the reference stars and its group is not
    // this one (`plan::reference_measurement_fold`; the gate resolves the same value).
    let reference_measurement_hash =
        reference_measurement_term(rc, group, reference_frame_id, reference_hash, prepare_path)?;
    for (idx, frame, path, is_reference) in snapshot {
        let (frame_hash, expected_hash) = member_registration_hash(
            rc,
            group,
            &frame,
            reference_frame_id,
            reference_hash,
            &reference_measurement_hash,
            prepare_path,
        )?;

        let mut reused: Option<RegisteredFrameOutcome> = None;
        if persist && !force_fresh {
            if let Some(row) = by_frame.get(&frame.frame_id) {
                if registration_row_is_fresh(row, reference_frame_id, &expected_hash) {
                    match PixelMap::from_json(row.transform_json.as_deref().unwrap_or_default()) {
                        Ok(star) => {
                            let registration_hash = row.config_hash.clone().unwrap_or_default();
                            // D22: a cached star map is recomposed on every
                            // read, so a moved grid or reference WCS never
                            // needs star detection again.
                            let composed = match &compose {
                                None => Ok((star, registration_hash)),
                                Some((grid, reference)) => {
                                    subject_dims(frame.width, frame.height, &path)
                                        .and_then(|dims| {
                                            compose_to_grid(&star, dims, reference, grid)
                                        })
                                        .map(|map| {
                                            let hash = grid_registered_hash(
                                                &registration_hash,
                                                &reference.hash,
                                                &grid.hash,
                                            );
                                            (map, hash)
                                        })
                                }
                            };
                            match composed {
                                Ok((map, registered_hash)) => {
                                    reused = Some(RegisteredFrameOutcome::Aligned {
                                        map,
                                        record: row.clone(),
                                        cached: true,
                                        registered_hash,
                                    });
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        run_id = rc.run_id,
                                        frame_id = frame.frame_id,
                                        error = %e,
                                        "grid composition of a cached star map failed; re-registering"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                run_id = rc.run_id,
                                frame_id = frame.frame_id,
                                error = %e,
                                "stored registration transform failed to parse; re-registering"
                            );
                        }
                    }
                }
            }
        }

        if let Some(outcome) = reused {
            // Perf tier A Task 6a fix round 1 (ruling R-TA-5): a cached
            // `registration_results` row can still be missing its OWN
            // materialized artifact — never written before this became
            // required (an existing catalog's first run under this cycle),
            // or removed by a cleanup. The CHECK stays here (a cheap
            // `find_artifact` SELECT + a disk `stat`, no pixel I/O); the
            // WARP+WRITE, when one is needed, is queued into
            // `needs_artifact_rewrite` and runs in its own fan-out below —
            // never one frame at a time on the run thread, which is exactly
            // what the previous round's `ensure_registered_artifact` did
            // and what made a from-scratch backfill add minutes to this
            // stage.
            // I2 fix wave: a rewrite-needed item's pixel work (warp +
            // write) has NOT happened yet — it must not count as done
            // here, or the Register row reads 100 % for the whole rewrite
            // batch below. Only a truly fresh reused item (or a Failed
            // outcome, which has no pixel work left to do) is counted
            // immediately; a rewrite item is counted when the rewrite
            // fan-out's own ticker ticks it.
            let mut needs_rewrite = false;
            if let RegisteredFrameOutcome::Aligned {
                map,
                record,
                registered_hash,
                ..
            } = &outcome
            {
                let hash = registered_hash.as_str();
                let row = registered_row(rc, &group.key, frame.frame_id).unwrap_or_else(|e| {
                    tracing::warn!(
                        run_id = rc.run_id,
                        frame_id = frame.frame_id,
                        error = ?e,
                        "failed to check the registered artifact's freshness; rewriting it"
                    );
                    None
                });
                let fresh = row.as_ref().is_some_and(|row| is_fresh(row, hash));
                if !fresh {
                    needs_rewrite = true;
                    // The write order: the row is gone before the file is replaced. A failed read still queues the
                    // rewrite, so the delete runs by key all the same (a point delete needs no row) and nothing is
                    // carried; a failed delete stops the job.
                    {
                        let conn = db(&rc.ctx)?.conn();
                        crate::db::stacking::delete_artifact(
                            &conn,
                            &rc.subject,
                            &group.key,
                            ArtifactKind::Registered,
                            Some(frame.frame_id),
                        )?;
                    }
                    if let Some(row) = row {
                        old_registered.insert(frame.frame_id, row);
                    }
                    let planes = rc
                        .measured
                        .get(&group.key)
                        .and_then(|v| v.get(idx))
                        .map(|e| e.planes)
                        .unwrap_or(1);
                    needs_artifact_rewrite.push(ArtifactRewriteItem {
                        frame_id: frame.frame_id,
                        calibrated: path.clone(),
                        frame: frame.clone(),
                        map: map.clone(),
                        rec: record.clone(),
                        registered_hash: registered_hash.clone(),
                        planes,
                    });
                }
            }
            if let Some(entries) = rc.measured.get_mut(&group.key) {
                entries[idx].registration = Some(outcome);
            }
            if !needs_rewrite {
                progress.0 += 1;
                rc.progress(
                    Stage::Register,
                    Some(group.key.clone()),
                    progress.0,
                    progress.1,
                    0,
                    0,
                    Some(frame.frame_id),
                    Some(message.to_string()),
                );
            }
        } else {
            // M4b Task 2: resolved only for the frames this pass will
            // actually register, so a fully cached group pays nothing for
            // the seed and the log line never claims a gate for a frame
            // that is not being judged.
            let (scale_gate, hint, policy) = {
                let conn = db(&rc.ctx)?.conn();
                registration_gate_and_hint(
                    solves,
                    &conn,
                    &origins,
                    &frame,
                    reference_frame_id,
                    reference_scale,
                )
            };
            tracing::debug!(
                run_id = rc.run_id,
                frame_id = frame.frame_id,
                scale_gate_low = scale_gate.0,
                scale_gate_high = scale_gate.1,
                hint = hint.is_some(),
                seed_policy = seed_policy_field(policy),
                "registration gate"
            );
            // Perf tier A Task 6a fix round 1: this frame's own plane count
            // (stage 3's `MeasuredFrame::planes`), threaded through so the
            // fan-out worker that writes the registered artifact never
            // needs `rc` to name the `_d` (debayered) output file.
            let planes = rc
                .measured
                .get(&group.key)
                .and_then(|v| v.get(idx))
                .map(|e| e.planes)
                .unwrap_or(1);
            // The write order, only for a pass that writes: a dry pass persists nothing and deletes nothing.
            if persist {
                let conn = db(&rc.ctx)?.conn();
                let old = crate::db::stacking::find_artifact(
                    &conn,
                    &rc.subject,
                    &group.key,
                    ArtifactKind::Registered,
                    Some(frame.frame_id),
                )?;
                crate::db::stacking::delete_artifact(
                    &conn,
                    &rc.subject,
                    &group.key,
                    ArtifactKind::Registered,
                    Some(frame.frame_id),
                )?;
                if let Some(old) = old {
                    old_registered.insert(frame.frame_id, old);
                }
            }
            to_register.push(PendingRegistration {
                idx,
                frame,
                path,
                is_reference,
                expected_hash,
                scale_gate,
                hint,
                policy,
                planes,
                calibrated_hash: frame_hash,
            });
        }
    }

    if to_register.is_empty() && needs_artifact_rewrite.is_empty() {
        if persist {
            note_low_inlier_frames(rc, group);
        }
        return Ok(pass);
    }

    // Registration warps every frame onto its GROUP's reference geometry
    // (already resolved by the caller — the run-wide one in co-registered
    // mode, the group's own in native) — the OUTPUT buffer size, and a
    // more accurate admission bound than any per-frame native geometry
    // would be. On a project's grid the group geometry is already the
    // grid's (`set_group_geometry` stores the delivered dimensions).
    // `reference_name` is the registered artifact's own
    // `ATH_REGR` card value (perf tier A Task 6a fix round 1: hoisted here,
    // once per group, instead of once per frame inside the old
    // `write_registered_artifact`).
    let (ref_w, ref_h, reference_name) = {
        let geometry = rc.geometry_of(&group.key);
        (
            geometry.width,
            geometry.height,
            geometry
                .calibrated
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("reference")
                .to_string(),
        )
    };
    // Perf tier A Task 6a fix round 1 (ruling R-TA-5): the register
    // fan-out's worker now also warps and writes the registered artifact
    // (`REGISTER_PLANES_RESIDENT`'s own doc has the measurement) —
    // `register_working_set_bytes` folds that cost in, channel-aware.
    //
    // Fix round 2 (ruling R-TA-6 M3): keyed on this batch's own MEASURED
    // plane count, not `group.color_mode` — an OSC group whose debayering
    // is off produces 1-plane frames, and charging it for 9 resident
    // planes it never actually holds would under-admit it for no reason.
    // Both batches share one geometry and one group, so one MAX over
    // whichever items exist covers them; `unwrap_or(1)` is unreachable in
    // practice (the caller already returned above when both are empty).
    let batch_planes = to_register
        .iter()
        .map(|p| p.planes)
        .chain(needs_artifact_rewrite.iter().map(|p| p.planes))
        .max()
        .unwrap_or(1);
    let working_set_bytes = register_working_set_bytes(ref_w as u64, ref_h as u64, batch_planes);
    let budget = fan_budget(rc);
    let admission_n = admission(
        working_set_bytes,
        rc.ctx.image_pool.current_num_threads(),
        budget.bytes,
    );
    log_admission(
        rc,
        Stage::Register,
        &group.key,
        working_set_bytes,
        0,
        admission_n,
        budget,
        "register",
    );
    // Perf tier A Task 6a fix round 1: both fan-outs below write into the
    // SAME directory — created once here rather than once per frame inside
    // the old `write_registered_artifact`. Flat since the frame-level cache
    // (spec §5.1): a frame's registered file does not move with its group.
    let out_dir = rc.layout.registered_dir();
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| RunError::Other(format!("creating {}: {e}", out_dir.display())))?;
    let interp = cfg.registration.interpolation;
    let clamping = cfg.registration.clamping_threshold;
    let run_id = rc.run_id;
    let subject = rc.subject.clone();

    // ── Cache-hit rewrite batch (ruling R-TA-5): parallel, admitted the
    // SAME way as the main fan-out below — no per-frame `rc` access, so it
    // never fights the main batch over `&mut RunContext`. Registration
    // itself is already known (`map`/`rec`); only the pixel work runs
    // here.
    //
    // I2 fix wave: this batch used to report NOTHING while it ran — the
    // `reused` loop above counted every reused frame (rewrite-needed or
    // not) as done up front, so the Register row sat at 100 % for the
    // whole rewrite (0.43 s mono / 1.26 s OSC per frame). It no longer
    // does that (see the loop's own comment); this batch gets its own
    // ticker instead, continuing from the SAME `progress.0` the main
    // batch's ticker below picks up from, so the two compose into one
    // running count. ──
    if !needs_artifact_rewrite.is_empty() {
        let pool_ref = &rc.ctx.image_pool;
        let out_dir_ref = &out_dir;
        let reference_name_ref = &reference_name;
        let grid_hash_ref = compose.as_ref().map(|(g, _)| g.hash.as_str());
        let cancel_ref: &AtomicBool = &rc.cancel;
        let rewrite_ticker = FanOutTicker::new(
            rc,
            Stage::Register,
            Some(group.key.clone()),
            progress.0,
            progress.1,
            0,
            0,
        )
        .with_message(message);
        let rewrite_ticker_ref = &rewrite_ticker;
        let rewrite_results = fan_out(
            needs_artifact_rewrite,
            &AdmissionGate::budgeted(rc, working_set_bytes, 0, admission_n),
            FanOutLabel::new(rc, Stage::Register, Some(&group.key), "rewrite"),
            cancel_ref,
            move |item: ArtifactRewriteItem| -> Result<(i64, String, Result<PathBuf, String>), String> {
                let frame_id = item.frame_id;
                let stem = frame_file_stem(&item.frame);
                let out = out_dir_ref.join(registered_file_name(&stem, item.planes == 3));
                let written = write_one_registered_frame(
                    &item.calibrated,
                    &item.map,
                    ref_w,
                    ref_h,
                    interp,
                    clamping,
                    reference_name_ref,
                    &item.rec,
                    grid_hash_ref,
                    Some(pool_ref),
                    &out,
                );
                let result = match written {
                    Ok(()) => Ok((item.frame_id, item.registered_hash, Ok(out))),
                    Err(e) => {
                        tracing::warn!(
                            run_id,
                            frame_id = item.frame_id,
                            error = ?e,
                            "failed to write registered frame"
                        );
                        Ok((item.frame_id, item.registered_hash, Err(format!("{e:#}"))))
                    }
                };
                rewrite_ticker_ref.tick(Some(frame_id));
                result
            },
        );
        rc.check_cancel()?;
        progress.0 = rewrite_ticker.done();
        for res in rewrite_results {
            match res {
                None => return Err(RunError::Cancelled),
                Some(Ok((frame_id, registered_hash, Ok(out_path)))) => {
                    if let Err(e) = record_registered_artifact(
                        rc,
                        &group.key,
                        frame_id,
                        &registered_hash,
                        &out_path,
                        old_registered.get(&frame_id),
                    ) {
                        tracing::warn!(
                            run_id = rc.run_id,
                            frame_id,
                            error = ?e,
                            "failed to record registered artifact"
                        );
                    }
                }
                // Write failed (already warned above) or a hard error the
                // closure never actually constructs — either way, nothing
                // to record; the frame falls back to the on-the-fly warp.
                Some(Ok((_, _, Err(error)))) => write_failures.note(error),
                Some(Err(_)) => {}
            }
        }
    }

    if to_register.is_empty() {
        write_failures.flush(rc, group);
        if persist {
            note_low_inlier_frames(rc, group);
        }
        return Ok(pass);
    }

    let meta: Vec<(usize, GroupFrame, String, SeedPolicy)> = to_register
        .iter()
        .map(|p| (p.idx, p.frame.clone(), p.expected_hash.clone(), p.policy))
        .collect();
    // Perf tier 1 Task 9: a `persist` item consumes (and frees) its dry-pass
    // cache entry here, on the run thread, before the fan-out spawns — the
    // dry pass (`persist = false`) never consults the cache itself (a fresh
    // pass always registers afresh, per this function's own doc comment),
    // so `pre` is always `None` there.
    let items: Vec<RegisterItem> = to_register
        .into_iter()
        .map(|p| {
            let pre = if persist {
                rc.dry_pass_stars.remove(&p.frame.frame_id)
            } else {
                None
            };
            // Tier C Task 3 (ruling C-6): resolved only when this item will
            // actually reach `detect_frame_stars` below — a `pre` hit and
            // the reference's own identity row never call it, so reading
            // the artifact for either would be wasted I/O.
            let fits = if pre.is_none() && !p.is_reference {
                resolve_register_fits(
                    rc,
                    &group.key,
                    p.frame.frame_id,
                    p.planes,
                    &p.calibrated_hash,
                )
            } else {
                None
            };
            RegisterItem {
                frame_id: p.frame.frame_id,
                frame: p.frame,
                path: p.path,
                is_reference: p.is_reference,
                scale_gate: p.scale_gate,
                hint: p.hint,
                policy: p.policy,
                pre,
                expected_hash: p.expected_hash,
                planes: p.planes,
                fits,
            }
        })
        .collect();

    // v0.6.3: per-frame ticks from inside the fan-out. `progress.0` already
    // counts this group's reused frames (emitted one by one above), so the
    // ticker continues from it; the results loop below then adopts the
    // ticker's count instead of re-counting — re-emitting from the
    // pre-fan-out value would run the bar backwards.
    let ticker = FanOutTicker::new(
        rc,
        Stage::Register,
        Some(group.key.clone()),
        progress.0,
        progress.1,
        0,
        0,
    )
    .with_message(message);
    let ticker_ref = &ticker;
    let cancel_ref: &AtomicBool = &rc.cancel;
    let pool_ref: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;
    let reg_cfg = &cfg.registration;
    let ref_stars_ref = ref_stars;
    // Perf tier A Task 6a fix round 1 (ruling R-TA-5): the registered
    // artifact's write now happens INSIDE this closure, right after a
    // successful alignment — see `RegisteredWrite`'s own doc.
    let out_dir_ref = &out_dir;
    let reference_name_ref = &reference_name;
    let compose_ref = &compose;

    let gate = AdmissionGate::budgeted(rc, working_set_bytes, 0, admission_n);
    let lbl = FanOutLabel::new(rc, Stage::Register, Some(&group.key), "align");
    let results = fan_out(items, &gate, lbl, cancel_ref, move |item: RegisterItem| {
        let out = if item.is_reference {
            Ok((identity_registration(ref_stars_ref), None))
        } else {
            // Perf tier 1 Task 9: `item.pre` is this frame's dry-pass
            // detection, already known — skip `detect_frame_stars`
            // entirely on a cache hit, exactly as `register_frame` (its
            // composition, kept intact for every OTHER caller) would do
            // for a cache miss. `was_miss` (fix round 1, Minor 2) has to
            // be read before the `match` below moves `item.pre`.
            let was_miss = item.pre.is_none();
            let detected: Result<Arc<DetectedStars>, IntegrationError> = match item.pre {
                Some(d) => Ok(d),
                None => {
                    detect_frame_stars(&item.path, reg_cfg, Some(pool_ref), item.fits.as_deref())
                        .map(Arc::new)
                }
            };
            match detected {
                Err(e) => Err(format!("registration failed: {e}")),
                Ok(d) => {
                    // Fix round 1, Minor 3: `register_detected` itself has
                    // no cancel awareness (it is pure CPU, by design) —
                    // `register_frame`'s own two checks (before the read,
                    // after the detect) covered this before the split.
                    // Checked between the detect and the align so a cancel
                    // raised mid-fan-out short-circuits before the
                    // (possibly expensive) RANSAC/distortion work starts;
                    // `rc.check_cancel()?` right after `fan_out` returns
                    // below is what actually stops the run either way, so
                    // this string is never shown to a user.
                    if cancel_ref.load(Ordering::Relaxed) {
                        Err("registration cancelled".to_string())
                    } else {
                        let mut reg = register_detected(
                            ref_stars_ref,
                            &d,
                            reg_cfg,
                            item.hint.as_ref(),
                            item.policy,
                            item.scale_gate,
                        );
                        // Fix round 1, Minor 2: `register_detected`'s own
                        // `duration_ms` is align-only (Task 9) — restore
                        // the miss arm's read+detect time onto what gets
                        // PERSISTED as `registration_results.compute_
                        // time_ms`, exactly as `register_frame` (every
                        // OTHER caller) still reports for a fresh
                        // detection. A cache hit (`!was_miss`) stays
                        // align-only (ruling 3) — the persisting pass paid
                        // no read/detect for it at all.
                        if was_miss {
                            reg.duration_ms += d.read_ms + d.detect_ms;
                        }
                        // Returned only for a dry pass (`!persist`) — the
                        // persisting pass consumed its cache entry
                        // building this very item (or never had one), so
                        // it has nothing new to cache.
                        Ok((reg, (!persist).then_some(d)))
                    }
                }
            }
        };
        // Perf tier A Task 6a fix round 1 (ruling R-TA-5): for a
        // PERSISTING item whose alignment just succeeded, build the
        // registration record AND write the registered artifact right
        // here — the full-frame warp this used to defer to the sequential
        // results loop is exactly the CPU cost `admission_n` exists to
        // parallelize. A dry pass, or an alignment failure, builds nothing
        // (`None`) — the sequential loop's existing handling for both is
        // unchanged.
        let out = out.and_then(|(reg, dry_stars)| -> Result<_, String> {
            let persisted = if persist {
                match &reg.outcome {
                    Ok(alignment) => {
                        let now =
                            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                        let is_reference_row = item.frame_id == reference_frame_id;
                        // The set id here is a placeholder: `store_registration`
                        // overwrites it with the real one (a project's record
                        // is its payload-only star-map artifact, plan 6b-3 D2).
                        let rec = to_record(
                            subject.frame_set_id().unwrap_or(0),
                            item.frame_id,
                            reference_frame_id,
                            is_reference_row,
                            &reg,
                            &item.expected_hash,
                            &now,
                        );
                        // Plan 6b-3 D1: the composed map is what the
                        // registered artifact is written with; `rec` keeps
                        // the STAR map's JSON (a composed map never reaches
                        // `to_record` or `to_json`).
                        let (map, registered_hash) = match compose_ref {
                            None => (alignment.map.clone(), item.expected_hash.clone()),
                            Some((grid, reference)) => {
                                let dims =
                                    subject_dims(item.frame.width, item.frame.height, &item.path)
                                        .map_err(|e| format!("grid composition failed: {e}"))?;
                                let map = compose_to_grid(&alignment.map, dims, reference, grid)
                                    .map_err(|e| format!("grid composition failed: {e}"))?;
                                let hash = grid_registered_hash(
                                    &item.expected_hash,
                                    &reference.hash,
                                    &grid.hash,
                                );
                                (map, hash)
                            }
                        };
                        let stem = frame_file_stem(&item.frame);
                        let out_path =
                            out_dir_ref.join(registered_file_name(&stem, item.planes == 3));
                        let (written, write_error) = match write_one_registered_frame(
                            &item.path,
                            &map,
                            ref_w,
                            ref_h,
                            interp,
                            clamping,
                            reference_name_ref,
                            &rec,
                            compose_ref.as_ref().map(|(g, _)| g.hash.as_str()),
                            Some(pool_ref),
                            &out_path,
                        ) {
                            Ok(()) => (Some(out_path), None),
                            Err(e) => {
                                tracing::warn!(
                                    run_id,
                                    frame_id = item.frame_id,
                                    error = ?e,
                                    "failed to write registered frame"
                                );
                                (None, Some(format!("{e:#}")))
                            }
                        };
                        Some(RegisteredWrite {
                            rec,
                            registered_hash,
                            written,
                            write_error,
                            map,
                        })
                    }
                    Err(_) => None,
                }
            } else {
                None
            };
            Ok((reg, dry_stars, persisted))
        });
        ticker_ref.tick(Some(item.frame_id));
        out
    });

    rc.check_cancel()?;
    progress.0 = ticker.done();

    for (pos, res) in results.into_iter().enumerate() {
        let (idx, frame, hash, policy) = &meta[pos];
        let (idx, policy) = (*idx, *policy);
        match res {
            None => return Err(RunError::Cancelled),
            Some(Err(msg)) => {
                if !persist {
                    pass.push(PassResult {
                        idx,
                        frame_id: frame.frame_id,
                        outcome: Err(msg),
                    });
                } else if cfg.selection.exclude_on_registration_failure {
                    rc.runtime_exclusions.push((frame.frame_id, msg.clone()));
                    if let Some(entries) = rc.measured.get_mut(&group.key) {
                        entries[idx].included = false;
                        entries[idx].reason = Some(msg.clone());
                        entries[idx].registration =
                            Some(RegisteredFrameOutcome::Failed(msg.clone()));
                    }
                    tracing::warn!(
                        run_id = rc.run_id,
                        frame_id = frame.frame_id,
                        reason = %msg,
                        "frame excluded"
                    );
                } else {
                    return Err(RunError::Other(msg.clone()));
                }
            }
            Some(Ok((reg, dry_stars, persisted))) => {
                // Perf tier 1 Task 9: a dry pass's own successful
                // detections are cached for the persisting pass right
                // after it — regardless of whether THIS pass's alignment
                // succeeded (a fresh detection is exactly what the
                // persisting pass needs even when the dry pass's own
                // probe against the OLD reference failed to align).
                if let Some(d) = dry_stars {
                    rc.dry_pass_stars.insert(frame.frame_id, d);
                }
                match &reg.outcome {
                    Ok(alignment) => {
                        if !persist {
                            pass.push(PassResult {
                                idx,
                                frame_id: frame.frame_id,
                                outcome: Ok((alignment.rotation_deg, alignment.translation)),
                            });
                        } else {
                            tracing::debug!(
                                run_id = rc.run_id,
                                frame_id = frame.frame_id,
                                inliers = alignment.inliers,
                                rms_px = alignment.rms_px,
                                "frame registered"
                            );
                            // Ruling R-T6-9: the quad matcher dropped this
                            // frame and the plate-solve seed picked it up. Its
                            // own note is in `warnings` below, but a distinct
                            // event is what makes the pattern countable — on
                            // real data it clusters by filter, not by frame.
                            if alignment.seed == SeedKind::Wcs && policy == SeedPolicy::QuadFirst {
                                tracing::warn!(
                                    run_id = rc.run_id,
                                    frame_id = frame.frame_id,
                                    "quad seed failed; the plate-solve seed carried this frame"
                                );
                            }
                            for note in &alignment.warnings {
                                tracing::warn!(
                                    run_id = rc.run_id,
                                    frame_id = frame.frame_id,
                                    note = %note,
                                    "registration warning"
                                );
                                rc.warnings
                                    .push(format!("frame {}: {note}", frame.frame_id));
                            }
                            // Perf tier A Task 6a fix round 1 (ruling
                            // R-TA-5): the record AND the registered
                            // artifact's pixel work were both already done
                            // inside the fan-out worker above — `persisted`
                            // is `Some` for every successfully-aligned,
                            // persisting item (the worker builds it
                            // unconditionally in that case; see the
                            // closure). Only the DB writes — this record's
                            // `registration_results` row and, when the
                            // write succeeded, the `registered` artifact
                            // row — stay here: `rusqlite` connections are
                            // not `Send` across `fan_out`'s plain threads.
                            let RegisteredWrite {
                                rec,
                                registered_hash,
                                written,
                                write_error,
                                map,
                            } = persisted.expect(
                                "a persisting, successfully-aligned item always carries a RegisteredWrite",
                            );
                            {
                                let conn = db(&rc.ctx)?.conn();
                                store_registration(&conn, &rc.subject, |set_id| {
                                    RegistrationRecord {
                                        frames_set_id: set_id,
                                        ..rec.clone()
                                    }
                                })?;
                            }
                            if let Some(error) = write_error {
                                write_failures.note(error);
                            }
                            if let Some(out_path) = &written {
                                if let Err(e) = record_registered_artifact(
                                    rc,
                                    &group.key,
                                    frame.frame_id,
                                    &registered_hash,
                                    out_path,
                                    old_registered.get(&frame.frame_id),
                                ) {
                                    tracing::warn!(
                                        run_id = rc.run_id,
                                        frame_id = frame.frame_id,
                                        error = ?e,
                                        "failed to record registered artifact"
                                    );
                                }
                            }
                            if let Some(entries) = rc.measured.get_mut(&group.key) {
                                entries[idx].registration = Some(RegisteredFrameOutcome::Aligned {
                                    map,
                                    record: rec,
                                    cached: false,
                                    registered_hash,
                                });
                            }
                        }
                    }
                    Err(align_err) => {
                        let reason = format!("registration failed: {align_err}");
                        if !persist {
                            pass.push(PassResult {
                                idx,
                                frame_id: frame.frame_id,
                                outcome: Err(reason),
                            });
                        } else {
                            let now = chrono::Utc::now()
                                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                            {
                                let conn = db(&rc.ctx)?.conn();
                                store_registration(&conn, &rc.subject, |set_id| {
                                    to_record(
                                        set_id,
                                        frame.frame_id,
                                        reference_frame_id,
                                        false,
                                        &reg,
                                        hash,
                                        &now,
                                    )
                                })?;
                            }
                            if cfg.selection.exclude_on_registration_failure {
                                rc.runtime_exclusions.push((frame.frame_id, reason.clone()));
                                if let Some(entries) = rc.measured.get_mut(&group.key) {
                                    entries[idx].included = false;
                                    entries[idx].reason = Some(reason.clone());
                                    entries[idx].registration =
                                        Some(RegisteredFrameOutcome::Failed(reason.clone()));
                                }
                                tracing::warn!(
                                    run_id = rc.run_id,
                                    frame_id = frame.frame_id,
                                    reason = %reason,
                                    "frame excluded"
                                );
                            } else {
                                return Err(RunError::Other(reason));
                            }
                        }
                    }
                }
            }
        }
    }
    // Robust quad seed RQ-6: with every outcome of the persisting pass in
    // (cached rows included), name the frames that kept under half the
    // group's median inliers. The two early exits above call it too.
    if persist {
        note_low_inlier_frames(rc, group);
    }
    // One settled tick after the bookkeeping, so the last throttled
    // in-flight tick is never the stage's final word.
    rc.progress(
        Stage::Register,
        Some(group.key.clone()),
        progress.0,
        progress.1,
        0,
        0,
        None,
        Some(message.to_string()),
    );

    write_failures.flush(rc, group);
    Ok(pass)
}

/// The Register progress total of a native run: every viable group's included members once, plus once more for
/// each group that will run a dry pass (the same way the co-registered path counts them). The dry pass is counted
/// only when, AFTER the identity check and the cache lookup (ruling R14 (b)), the decision is not cached and the
/// group can still switch (ruling R15): a member the check excluded may have been the only frame the re-pick could
/// land on, and `two_pass_refine` then skips the pass.
fn native_register_total(
    rc: &mut RunContext,
    cfg: &StackingConfig,
    manual_reference_frame_id: Option<i64>,
    force_fresh: bool,
) -> Result<usize, RunError> {
    let groups = rc.plan_groups.clone();
    let mut total = 0usize;
    for g in &groups {
        if included_count_of(rc, &g.key) < 3 {
            continue;
        }
        let (group_reference, group_reference_hash) = {
            let geometry = rc.geometry_of(&g.key);
            (geometry.reference_frame_id, geometry.hash.clone())
        };
        if group_two_pass_wanted(cfg, group_reference, manual_reference_frame_id)
            && group_can_switch(rc, &g.key, group_reference)
        {
            let Some(lookup) = two_pass_lookup_after_identity_check(
                rc,
                g,
                group_reference,
                &group_reference_hash,
                force_fresh,
            )?
            else {
                continue;
            };
            let included = included_count_of(rc, &g.key);
            total += included;
            if lookup.cached.is_none() && group_can_switch(rc, &g.key, group_reference) {
                total += included;
            }
        } else {
            total += included_count_of(rc, &g.key);
        }
    }
    Ok(total)
}

/// Stage 5 in `native` geometry mode (M4b rulings R-M4b-4/5): every group
/// registers onto ITS OWN reference, in its own geometry, with no
/// cross-group registration at all.
///
/// Per viable group, in plan order: read that group's reference's stars
/// (which fixes the geometry its master, its `.athln` sidecars, its
/// rejection bitmaps and its drizzled output are all delivered in),
/// optionally run the two-pass re-pick over the group's OWN members, then
/// register the group persistently against whatever came out. A switch in
/// the group that also holds the RUN's reference moves the run-level
/// record with it (`stacking_runs.reference_frame_id` stays the largest
/// group's reference, ruling R-M4b-4).
///
/// The co-registered path's one up-front `reference_stars` read has no
/// counterpart here on purpose: each group reads its own, and the run-wide
/// reference is always some group's reference too, so nothing is detected
/// twice.
fn register_native_groups(rc: &mut RunContext) -> Result<(), RunError> {
    let cfg = rc.config.clone();
    let run_reference_frame_id = rc
        .reference_frame_id
        .ok_or_else(|| RunError::Other("no reference frame chosen".to_string()))?;
    let manual_reference_frame_id =
        (cfg.reference.mode == ReferenceMode::Manual).then_some(run_reference_frame_id);

    let by_frame: HashMap<i64, RegistrationRecord> = {
        let conn = db(&rc.ctx)?.conn();
        stored_registrations(&conn, &rc.subject)?
            .into_iter()
            .map(|r| (r.frame_id, r))
            .collect()
    };

    let groups = rc.plan_groups.clone();
    let force_fresh = stage_forces_fresh(rc.rerun_from, Stage::Register);
    let origins = rc.origins.clone();
    let mut solves: SolveCache = SolveCache::new();

    let total = native_register_total(rc, &cfg, manual_reference_frame_id, force_fresh)?;
    let mut progress = (0usize, total);
    rc.progress(
        Stage::Register,
        None,
        progress.0,
        progress.1,
        0,
        0,
        None,
        None,
    );

    for group in &groups {
        rc.check_cancel()?;
        if included_count_of(rc, &group.key) < 3 {
            continue;
        }

        let geometry = rc.geometry_of(&group.key).clone();
        let mut reference_frame_id = geometry.reference_frame_id;
        let mut reference_hash = geometry.hash.clone();
        // Tier C Task 3 (ruling C-6): `geometry.hash` IS this group's
        // reference's own stage-1 hash already — no extra lookup needed to
        // resolve its `fits` artifact, unlike the co-registered path above.
        let reference_fits = resolve_reference_fits(rc, reference_frame_id, &reference_hash);
        fail_if_reference_changed(rc, reference_frame_id)?;
        let mut ref_stars = {
            let pool_ref = &rc.ctx.image_pool;
            reference_stars(
                &geometry.calibrated,
                &cfg.registration,
                Some(pool_ref),
                reference_fits.as_deref(),
            )
            .map_err(|e| RunError::Other(format!("reference star detection failed: {e}")))?
        };
        set_group_geometry(
            rc,
            &group.key,
            reference_frame_id,
            &ref_stars,
            &geometry.calibrated,
            &reference_hash,
        );

        // Every cross-scale frame of this group needs its reference's own
        // stored solve; warming the cache once here keeps the per-frame
        // lookups to one query for the whole group (including the negative
        // answer).
        {
            let conn = db(&rc.ctx)?.conn();
            solve_of(&mut solves, &conn, &origins, reference_frame_id);
        }

        if group_two_pass_wanted(&cfg, reference_frame_id, manual_reference_frame_id)
            && group_can_switch(rc, &group.key, reference_frame_id)
        {
            if let Some(switch) = two_pass_refine(
                rc,
                group,
                &ref_stars,
                reference_frame_id,
                &reference_hash,
                &by_frame,
                force_fresh,
                &mut progress,
                &mut solves,
            )? {
                let previous = reference_frame_id;
                reference_frame_id = switch.frame_id;
                ref_stars = switch.stars;
                reference_hash = switch.hash;
                set_group_geometry(
                    rc,
                    &group.key,
                    reference_frame_id,
                    &ref_stars,
                    &switch.calibrated,
                    &reference_hash,
                );
                // The run-level reference is the largest group's (ruling
                // R-M4b-4) — when THAT group's own reference moves, the run
                // row and the summary header move with it; every other
                // group's switch is a per-group fact only.
                if previous == run_reference_frame_id {
                    {
                        let conn = db(&rc.ctx)?.conn();
                        set_run_reference(
                            &conn,
                            rc.run_id,
                            reference_frame_id,
                            reference_mode_wire(cfg.reference.mode),
                        )?;
                    }
                    rc.reference_frame_id = Some(reference_frame_id);
                    rc.reference_calibrated = Some(switch.calibrated.clone());
                    rc.reference_width = ref_stars.width;
                    rc.reference_height = ref_stars.height;
                    rc.summary.reference.frame_id = Some(reference_frame_id);
                    rc.summary.reference.filename = Some(switch.filename);
                    rc.summary.reference.weight = switch.weight;
                    rc.summary.reference.switched_from = Some(switch.from);
                }
            }
        }

        register_group_pass(
            rc,
            group,
            &ref_stars,
            reference_frame_id,
            &reference_hash,
            &by_frame,
            force_fresh,
            true,
            &mut progress,
            &mut solves,
        )?;
    }

    Ok(())
}

/// Rewrite ONE group's [`GroupGeometry`] from its resolved reference (M4b
/// native mode): the frame, the geometry its own `reference_stars` read
/// reports, its calibrated file and its stage-1 hash. Also refreshes the
/// run-wide `reference_width`/`height` when this group's reference is the
/// run's, so the two never disagree about the same frame. On a grid the
/// group's delivered dimensions are the grid's, not the reference's.
fn set_group_geometry(
    rc: &mut RunContext,
    key: &str,
    reference_frame_id: i64,
    ref_stars: &ReferenceStars,
    calibrated: &Path,
    hash: &str,
) {
    let (width, height) = delivered_dims(rc, (ref_stars.width, ref_stars.height));
    if let Some(entry) = rc.group_geometry.get_mut(key) {
        entry.reference_frame_id = reference_frame_id;
        entry.width = width;
        entry.height = height;
        entry.calibrated = calibrated.to_path_buf();
        entry.hash = hash.to_string();
    }
    if rc.reference_frame_id == Some(reference_frame_id) {
        rc.reference_width = ref_stars.width;
        rc.reference_height = ref_stars.height;
    }
}

/// The geometry a group is delivered in (plan 6b-3): the grid's on a
/// project run, else the group reference's own `native` dimensions.
pub(super) fn delivered_dims(rc: &RunContext, native: (usize, usize)) -> (usize, usize) {
    rc.grid.as_ref().map(|g| g.dims()).unwrap_or(native)
}

/// Stage 5 (register, spec §3, ruling 10): register every included frame of
/// every VIABLE group (≥ 3 included, per stage 3) onto the ONE global
/// reference (its stars computed once), reusing an existing
/// `registration_results` row when it is still fresh
/// ([`registration_row_is_fresh`], the SAME predicate the plan's own
/// `stale_stages` uses — ruling 10), fanning the rest out. Writes every
/// group's every frame's `stacking_run_frames` row at the end
/// ([`write_frame_rows`]), included or not.
///
/// M4a Task 4 (ruling R-M4a-5): in `Auto` mode with `reference.two_pass`
/// on, the reference's OWN group is registered once first through
/// [`register_group_pass`] with `persist = false` — a dry pass whose only
/// product is each frame's rotation/translation against the stage-4 pick.
/// [`two_pass_pick`] then re-chooses the reference among the top-weighted
/// of those frames by corner displacement from that group's median
/// transform, and the real, persisting loop below runs with whatever came
/// out (re-detecting the new reference's stars and recomputing its stage-1
/// hash first, so every row the second pass writes names the FINAL
/// reference). Both passes live inside ONE `Stage::Register` timing and
/// share ONE progress total. A `Manual` reference never moves.
pub(super) fn stage_register(rc: &mut RunContext) -> Result<(), RunError> {
    let stage_start = Instant::now();
    let meter = crate::stacking::telemetry::StageMeter::start(rc, Stage::Register, None);
    let cfg = rc.config.clone();

    if cfg.registration.geometry == RegistrationGeometry::Native {
        register_native_groups(rc)?;
        reweigh_groups(rc)?;
        write_frame_rows(rc)?;
        meter.finish();
        rc.timings.push(crate::stacking::provenance::StageTiming {
            stage: Stage::Register,
            duration_ms: stage_start.elapsed().as_millis() as u64,
        });
        // Perf tier 1 Task 9, belt and braces: `register_group_pass`
        // already drains its own dry-pass entries as the persisting pass
        // consumes them (per group, since native mode runs its dry pass
        // through the SAME function once per group), but a group that
        // never reaches a persisting pass — cancelled mid-stage, or with
        // fewer than 3 included frames after the dry pass excluded one —
        // would otherwise leave its entries stranded until the run ends.
        rc.dry_pass_stars.clear();
        return Ok(());
    }

    let mut reference_frame_id = rc
        .reference_frame_id
        .ok_or_else(|| RunError::Other("no reference frame chosen".to_string()))?;
    let reference_calibrated = rc
        .reference_calibrated
        .clone()
        .ok_or_else(|| RunError::Other("reference frame has no calibrated file".to_string()))?;

    // Tier C Task 3 (ruling C-6): the reference's own calibration hash is
    // resolved BEFORE the star read now (it used to follow it) so a fresh
    // mono `fits` artifact can be looked up and handed to `reference_stars`
    // instead of a fresh detection — `calibration_hash_checked` is memoized,
    // so hoisting this costs nothing new.
    let reference_group_frame = find_frame_in_groups(&rc.plan_groups, reference_frame_id)
        .ok_or_else(|| RunError::Other("reference frame not found in any group".to_string()))?;
    let mut reference_hash = {
        let conn = db(&rc.ctx)?.conn();
        rc.memo
            .calibration_hash_checked(&conn, &cfg, &reference_group_frame)?
    };
    let reference_fits = resolve_reference_fits(rc, reference_frame_id, &reference_hash);

    // The identity check comes before the reference's star read, so that
    // read never sees a changed landed file.
    fail_if_reference_changed(rc, reference_frame_id)?;
    let mut ref_stars = {
        let pool_ref = &rc.ctx.image_pool;
        reference_stars(
            &reference_calibrated,
            &cfg.registration,
            Some(pool_ref),
            reference_fits.as_deref(),
        )
        .map_err(|e| RunError::Other(format!("reference star detection failed: {e}")))?
    };
    rc.reference_width = ref_stars.width;
    rc.reference_height = ref_stars.height;

    // M4b ruling R-M4b-5: co-registered means every group's geometry IS the
    // run-wide reference's — the measured dimensions stage 4 recorded are
    // replaced here by the authoritative `reference_stars` read.
    adopt_co_registered_geometry(rc, &reference_calibrated, &reference_hash);

    let by_frame: HashMap<i64, RegistrationRecord> = {
        let conn = db(&rc.ctx)?.conn();
        stored_registrations(&conn, &rc.subject)?
            .into_iter()
            .map(|r| (r.frame_id, r))
            .collect()
    };

    let groups = rc.plan_groups.clone();
    let force_fresh = stage_forces_fresh(rc.rerun_from, Stage::Register);

    // M4b Task 2: one stored-solve lookup per frame for the WHOLE stage,
    // shared by the dry pass and the persisting one. The reference's own
    // row goes in first — every cross-scale frame's seed needs it.
    let origins = rc.origins.clone();
    let mut solves: SolveCache = SolveCache::new();
    {
        let conn = db(&rc.ctx)?.conn();
        solve_of(&mut solves, &conn, &origins, reference_frame_id);
    }

    // The dry pass's own group (ruling R-M4a-5): Auto mode, two-pass on,
    // and the reference's group viable — a group the persisting loop would
    // skip anyway gives nothing to take a median over.
    let dry_group: Option<IntegrationGroup> =
        if cfg.reference.mode == ReferenceMode::Auto && cfg.reference.two_pass {
            groups
                .iter()
                .find(|g| {
                    g.frames.iter().any(|f| f.frame_id == reference_frame_id)
                        && included_count_of(rc, &g.key) >= 3
                        && group_can_switch(rc, &g.key, reference_frame_id)
                })
                .cloned()
        } else {
            None
        };
    // A cached two-pass decision removes the dry pass from the total (frame-level cache spec §5.6).
    // The lookup runs after the identity check, as `two_pass_refine` does (ruling R14 (b)), so the totals count the
    // members that will register.
    let dry_total = match dry_group.as_ref() {
        Some(g) => {
            match two_pass_lookup_after_identity_check(rc, g, reference_frame_id, &reference_hash, force_fresh)? {
                Some(lookup) if lookup.cached.is_none() && group_can_switch(rc, &g.key, reference_frame_id) => {
                    included_count_of(rc, &g.key)
                }
                _ => 0,
            }
        }
        None => 0,
    };
    let mut viable_total = 0usize;
    for g in &groups {
        let included = included_count_of(rc, &g.key);
        if included >= 3 {
            viable_total += included;
        }
    }

    let mut progress = (0usize, viable_total + dry_total);
    rc.progress(
        Stage::Register,
        None,
        progress.0,
        progress.1,
        0,
        0,
        None,
        None,
    );

    if let Some(group) = &dry_group {
        rc.check_cancel()?;
        if let Some(switch) = two_pass_refine(
            rc,
            group,
            &ref_stars,
            reference_frame_id,
            &reference_hash,
            &by_frame,
            force_fresh,
            &mut progress,
            &mut solves,
        )? {
            {
                let conn = db(&rc.ctx)?.conn();
                set_run_reference(
                    &conn,
                    rc.run_id,
                    switch.frame_id,
                    reference_mode_wire(cfg.reference.mode),
                )?;
            }

            reference_frame_id = switch.frame_id;
            ref_stars = switch.stars;
            reference_hash = switch.hash;
            rc.reference_width = ref_stars.width;
            rc.reference_height = ref_stars.height;
            rc.reference_frame_id = Some(switch.frame_id);
            rc.reference_calibrated = Some(switch.calibrated.clone());
            rc.summary.reference.frame_id = Some(switch.frame_id);
            rc.summary.reference.filename = Some(switch.filename);
            rc.summary.reference.weight = switch.weight;
            rc.summary.reference.switched_from = Some(switch.from);
            // Co-registered: every group registers onto the run's ONE
            // reference, so the switch moves every group's geometry with it
            // (ruling R-M4b-5).
            adopt_co_registered_geometry(rc, &switch.calibrated, &reference_hash);
        }
    }

    for group in &groups {
        rc.check_cancel()?;
        register_group_pass(
            rc,
            group,
            &ref_stars,
            reference_frame_id,
            &reference_hash,
            &by_frame,
            force_fresh,
            true,
            &mut progress,
            &mut solves,
        )?;
    }

    reweigh_groups(rc)?;
    write_frame_rows(rc)?;

    meter.finish();
    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Register,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    // Perf tier 1 Task 9, belt and braces (see the native-mode return
    // above for why this can't rely solely on the per-item drain in
    // `register_group_pass`).
    rc.dry_pass_stars.clear();

    Ok(())
}

/// Perf tier A Task 6a fix round 1 (ruling R-TA-5): the registered
/// artifact's PIXEL work ONLY — source cards, the warp, the tmp+atomic-
/// rename write — with no `RunContext`/DB access at all, so BOTH register
/// fan-out closures (the main align-and-write one, and the cache-hit
/// rewrite-only one) can call it directly from a worker thread. `rec`'s
/// `model`/`transform_json`/`rms_residual_px` (already resolved by
/// [`to_record`]) feed the `ATH_REG*` provenance cards — see
/// [`RegisteredCards`]'s own doc for what each one means. The registered
/// artifact is a REQUIRED per-frame output since this task — Normalize and
/// Integrate both read it in place of re-warping the calibrated frame on
/// every band (`RegisteredSource::open_materialized`).
///
/// The config field `registration.write_registered_frames` (Settings →
/// Stacking, `RegisterPanel`'s "Write registered frames" toggle) no longer
/// gates this write — kept only because removing it would be a
/// command-surface change out of this task's scope; it is otherwise inert.
#[allow(clippy::too_many_arguments)]
fn write_one_registered_frame(
    calibrated: &Path,
    map: &PixelMap,
    ref_w: usize,
    ref_h: usize,
    interp: Interpolation,
    clamping: f32,
    reference_name: &str,
    rec: &RegistrationRecord,
    grid_hash: Option<&str>,
    pool: Option<&Arc<rayon::ThreadPool>>,
    out: &Path,
) -> anyhow::Result<()> {
    let source = source_cards_from_file(calibrated)?;
    let cards = build_registered_cards(
        &source,
        &RegisteredCards {
            reference_name,
            model: rec.model.as_deref().unwrap_or_default(),
            transform_json: rec.transform_json.as_deref().unwrap_or_default(),
            interpolation: interp,
            clamping,
            rms_px: rec.rms_residual_px,
            // D3: a grid frame states the grid's own row order, never the
            // reference's.
            reference_roworder: grid_hash.map(|_| "BOTTOM-UP"),
            grid_hash,
        },
    )?;
    write_registered_frame(
        calibrated, map, ref_w, ref_h, interp, clamping, &cards, pool, out,
    )?;
    Ok(())
}

/// Perf tier A Task 6a fix round 1: the registered artifact's DB
/// bookkeeping ONLY — `file_identity` + the `registered` artifact row,
/// keyed on `registered_hash` (plan 6b-3 D2): the registration hash, already
/// folded with the frame's own calibration hash by `registration_hash_for`
/// — a re-registration or a re-calibration invalidates it exactly like the
/// LN `.athln` sidecars — or, on a grid, the composed hash that also folds
/// the reference's WCS and the grid. Split from the pixel work
/// ([`write_one_registered_frame`]) because `rusqlite` connections are not
/// `Send` across `fan_out`'s plain `thread::scope` workers — this runs on
/// the run thread, in the sequential results loop, right after the write
/// a worker already finished.
fn record_registered_artifact(
    rc: &mut RunContext,
    group_key: &str,
    frame_id: i64,
    registered_hash: &str,
    out: &Path,
    old: Option<&crate::db::stacking::StackingArtifactRow>,
) -> anyhow::Result<()> {
    let (size, modified_at) = file_identity(out)?;
    let conn = db(&rc.ctx)?.conn();
    // `old` is the row the queue site read and deleted BEFORE the file was replaced (the write order, file-identity
    // spec §4.2); it only tells `replace_frame_level_file` which legacy file to retire.
    upsert_artifact(
        &conn,
        &NewArtifact {
            subject: &rc.subject,
            frame_id: Some(frame_id),
            group_key,
            kind: ArtifactKind::Registered,
            path: out.to_str(),
            config_hash: registered_hash,
            size: Some(size),
            modified_at: Some(&modified_at),
            payload_json: None,
        },
    )?;
    replace_frame_level_file(&conn, &rc.layout.registered_root(), old, out);
    Ok(())
}

/// Perf tier A Task 6a fix round 1: the CHEAP half of what used to be
/// `ensure_registered_artifact` — a `find_artifact` SELECT, judged by the
/// caller with [`is_fresh`] (a disk `stat`, no pixel I/O) — called from
/// `register_group_pass`'s cache-hit branch to decide whether a frame needs
/// queuing into `needs_artifact_rewrite`. The expensive half (warp + write)
/// now runs in that batch's own fan-out, not here. The row comes back so a
/// queued rewrite can carry it to `record_registered_artifact` after the
/// delete of its key (file-identity spec §4.2).
fn registered_row(
    rc: &RunContext,
    group_key: &str,
    frame_id: i64,
) -> anyhow::Result<Option<crate::db::stacking::StackingArtifactRow>> {
    let conn = db(&rc.ctx)?.conn();
    crate::db::stacking::find_artifact(
        &conn,
        &rc.subject,
        group_key,
        ArtifactKind::Registered,
        Some(frame_id),
    )
}

/// `r_<stem>[_d].fits` for the SAME frame-only `stem`
/// [`frame_file_stem`] gave the calibrated file (fix round 1: the two
/// naming schemes must agree by construction). `debayered` is the caller's
/// own known fact (`MeasuredFrame::planes == 3`) — NOT inferred by
/// checking whether the calibrated file's own on-disk name ends in `_d`
/// (fix round 1's addendum: a SOURCE light whose own filename already ends
/// in `_d`, e.g. a mono `vega_d.fits`, made that heuristic misfire —
/// `c_vega_d.fits` "looks" debayered by name alone even though it is not).
/// Matches `register_probe.rs`'s own convention of keeping the marker on
/// the registered output too.
fn registered_file_name(stem: &str, debayered: bool) -> String {
    if debayered {
        format!("r_{stem}_d.fits")
    } else {
        format!("r_{stem}.fits")
    }
}

#[cfg(test)]
#[path = "register_tests.rs"]
mod tests;
