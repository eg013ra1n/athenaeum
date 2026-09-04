//! File organizer for export operations
//!
//! Creates folder structures and copies/symlinks files for PixInsight WBPP.
//!
//! The folder hierarchy encodes the calibration pipeline — parent folder's
//! frames calibrate child folder's frames. WBPP reads this via "Grouping
//! Keywords with Pre".
//!
//! Full hierarchy (when all calibrations exist):
//! ```text
//! camera_{instrume}/
//!   BIAS_{bias_set_id}/
//!     bias frames...
//!     DARKS_{dark_set_id}/
//!       dark frames + darkflat frames...
//!       FLAT_{flat_set_id}/
//!         flat frames...
//!         lights/
//!           light frames...
//! ```
//!
//! Missing calibration levels are simply omitted (collapsed).

use crate::export::models::{
    calibrated_output_filename, sanitize_display_folder_name, sanitize_folder_name,
    CalibrationSetInfo, CalibrationSubgroup, ExportData, ExportProgressEvent, WbppExportConfig,
};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use crate::events::{ProgressEmitter, emit_event};

/// Result of organizing files for export
#[derive(Debug, Clone)]
pub struct OrganizeResult {
    pub files_organized: i32,
    pub warnings: Vec<String>,
}

/// Count total files that will be organized (for progress tracking).
/// Mirrors the logic of `organize_subgroup` to count without copying.
fn count_total_files(data: &ExportData) -> usize {
    let mut total = 0usize;
    let mut counted_set_ids: HashSet<i64> = HashSet::new();

    for group in &data.groups {
        for subgroup in &group.subgroups {
            // Count calibration frames (same dedup logic as organize_subgroup)
            let bias = subgroup
                .bias
                .as_ref()
                .or_else(|| subgroup.dark.as_ref().and_then(|d| d.bias.as_deref()));
            let dark = subgroup.dark.as_ref();
            let flat = subgroup.flat.as_ref();
            let dark_flat = flat.and_then(|f| f.dark_flat.as_deref());
            let flat_dark = flat.and_then(|f| f.dark.as_deref());
            let flat_bias = flat.and_then(|f| f.bias.as_deref());

            // Bias frames
            if let Some(b) = bias {
                if counted_set_ids.insert(b.set_id) {
                    total += b.frames.len();
                }
            }
            if let Some(fb) = flat_bias {
                if counted_set_ids.insert(fb.set_id) {
                    total += fb.frames.len();
                }
            }
            // Dark frames
            if let Some(d) = dark {
                if counted_set_ids.insert(d.set_id) {
                    total += d.frames.len();
                }
                if let Some(ref db) = d.bias {
                    if counted_set_ids.insert(db.set_id) {
                        if bias.is_none() && flat_bias.is_none() {
                            total += db.frames.len();
                        }
                    }
                }
            }
            if let Some(fd) = flat_dark {
                if counted_set_ids.insert(fd.set_id) {
                    total += fd.frames.len();
                }
            }
            if let Some(df) = dark_flat {
                if counted_set_ids.insert(df.set_id) {
                    total += df.frames.len();
                }
            }
            // Flat frames
            if let Some(f) = flat {
                if counted_set_ids.insert(f.set_id) {
                    total += f.frames.len();
                }
            }
            // Light frames
            total += subgroup.frames.len();
        }
    }
    total
}

/// One frame's placement in the WBPP hierarchy — the pure (no-I/O) description of
/// where the organizer would put a frame.
///
/// `rel_dir` is the directory **relative to the per-frame-set root**, always
/// forward-slash (e.g. `camera_ASI/BIAS_5/DARKS_6/FLAT_7/lights`). The frame-set
/// name is deliberately NOT part of it: the export path prepends its own
/// `sanitize_display_folder_name(frame_set_name)` level, and the Transfers send
/// path leaves the top level to the receiver's `<batch_name>` landing dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WbppPlacement {
    pub frame_id: i64,
    pub file_path: String,
    pub filename: String,
    /// Forward-slash directory relative to the frame-set root (frame-set name NOT
    /// included).
    pub rel_dir: String,
    /// Where this placement's bytes come from — copied, or calibrated into
    /// place. See [`PlacementSource`].
    pub source: PlacementSource,
}

/// Where a placement's bytes come from.
///
/// `Copy` is every mode but one: the file at `file_path` is copied (or
/// symlinked) as it is. `CalibrateLight` is the calibrated-lights mode: the
/// file at `file_path` is the RAW light, and the executor generates the
/// destination from it and its linked masters.
///
/// Derived from [`crate::export::models::ExportFrame::debayer_calibrated`],
/// which only the calibrated-lights mode transform sets — so every other
/// caller of [`compute_wbpp_placements`] keeps seeing `Copy` for everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementSource {
    Copy,
    CalibrateLight {
        frame_id: i64,
        /// Whether the generated output is debayered. Mirrors the `_d` marker
        /// already in `filename` — the transform decided both from one value.
        debayer: bool,
    },
}

/// Compute the WBPP placement of every frame the organizer would place, in the
/// same traversal order and with the same per-set dedup semantics as
/// [`organize_files_wbpp`] — but WITHOUT any filesystem I/O.
///
/// This is the single source of truth for the WBPP hierarchy: the export path
/// ([`organize_files_wbpp`]) copies/symlinks each returned placement, and the
/// Transfers object-send path ([`crate::api::sync`]) uses the same `rel_dir` to
/// lay a package out on the wire. Calibration frames are deduped across subgroups
/// (each set placed once); light frames are always placed at their `lights/`
/// level. Missing calibration levels collapse exactly as the organizer does.
pub fn compute_wbpp_placements(data: &ExportData) -> Vec<WbppPlacement> {
    let mut out = Vec::new();
    let mut organized_set_ids: HashSet<i64> = HashSet::new();
    for group in &data.groups {
        for subgroup in &group.subgroups {
            push_subgroup_placements(subgroup, &mut organized_set_ids, &mut out);
        }
    }
    out
}

/// Append the placements for one subgroup, mutating `organized_set_ids` with the
/// same cross-subgroup dedup the organizer uses. Mirrors the level decisions of
/// the copying path 1:1 (BIAS → DARKS → FLAT → lights); the only difference is it
/// records placements instead of touching the disk.
fn push_subgroup_placements(
    subgroup: &CalibrationSubgroup,
    organized_set_ids: &mut HashSet<i64>,
    out: &mut Vec<WbppPlacement>,
) {
    // Camera name from the first light frame (matches the copying path).
    let camera_name = subgroup
        .frames
        .first()
        .and_then(|f| f.instrume.as_ref())
        .map(|s| sanitize_folder_name(s))
        .unwrap_or_else(|| "unknown".to_string());

    // Resolve the effective calibration sets for this subgroup.
    let bias: Option<&CalibrationSetInfo> = subgroup
        .bias
        .as_ref()
        .or_else(|| subgroup.dark.as_ref().and_then(|d| d.bias.as_deref()));
    let dark: Option<&CalibrationSetInfo> = subgroup.dark.as_ref();
    let flat: Option<&CalibrationSetInfo> = subgroup.flat.as_ref();
    let dark_flat: Option<&CalibrationSetInfo> = flat.and_then(|f| f.dark_flat.as_deref());
    let flat_dark: Option<&CalibrationSetInfo> = flat.and_then(|f| f.dark.as_deref());
    let flat_bias: Option<&CalibrationSetInfo> = flat.and_then(|f| f.bias.as_deref());

    let mut dir: Vec<String> = vec![format!("camera_{}", camera_name)];

    // Level 1: BIAS (outermost calibration)
    if let Some(bias_info) = bias {
        dir.push(format!("BIAS_{}", bias_info.set_id));
        if organized_set_ids.insert(bias_info.set_id) {
            place_at(&bias_info.frames, &dir, out);
        }
        if let Some(fb) = flat_bias {
            if fb.set_id != bias_info.set_id && organized_set_ids.insert(fb.set_id) {
                place_at(&fb.frames, &dir, out);
            }
        }
    } else if let Some(fb) = flat_bias {
        dir.push(format!("BIAS_{}", fb.set_id));
        if organized_set_ids.insert(fb.set_id) {
            place_at(&fb.frames, &dir, out);
        }
    }

    // Level 2: DARKS
    if dark.is_some() || dark_flat.is_some() || flat_dark.is_some() {
        let darks_set_id = dark
            .map(|d| d.set_id)
            .or_else(|| flat_dark.map(|d| d.set_id))
            .or_else(|| dark_flat.map(|df| df.set_id))
            .unwrap_or(0);
        dir.push(format!("DARKS_{}", darks_set_id));

        if let Some(dark_info) = dark {
            if organized_set_ids.insert(dark_info.set_id) {
                place_at(&dark_info.frames, &dir, out);
            }
            if let Some(ref dark_bias) = dark_info.bias {
                if organized_set_ids.insert(dark_bias.set_id)
                    && bias.is_none()
                    && flat_bias.is_none()
                {
                    place_at(&dark_bias.frames, &dir, out);
                }
            }
        }

        if let Some(fd) = flat_dark {
            if organized_set_ids.insert(fd.set_id) {
                place_at(&fd.frames, &dir, out);
            }
        }

        if let Some(df_info) = dark_flat {
            if organized_set_ids.insert(df_info.set_id) {
                place_at(&df_info.frames, &dir, out);
            }
        }
    }

    // Level 3: FLAT
    if let Some(flat_info) = flat {
        dir.push(format!("FLAT_{}", flat_info.set_id));
        if organized_set_ids.insert(flat_info.set_id) {
            place_at(&flat_info.frames, &dir, out);
        }
    }

    // Innermost: lights/ (always placed, never deduped).
    dir.push("lights".to_string());
    place_at(&subgroup.frames, &dir, out);
}

/// Record one placement per frame under the forward-slash directory `dir`.
fn place_at(
    frames: &[crate::export::models::ExportFrame],
    dir: &[String],
    out: &mut Vec<WbppPlacement>,
) {
    let rel_dir = dir.join("/");
    for f in frames {
        out.push(WbppPlacement {
            frame_id: f.frame_id,
            file_path: f.file_path.clone(),
            filename: f.filename.clone(),
            rel_dir: rel_dir.clone(),
            source: match f.debayer_calibrated {
                None => PlacementSource::Copy,
                Some(debayer) => PlacementSource::CalibrateLight {
                    frame_id: f.frame_id,
                    debayer,
                },
            },
        });
    }
}

/// Destinations claimed within one export run, keyed case-insensitively —
/// on NTFS/APFS `L_0001.fits` and `l_0001.FITS` are ONE file, so the second
/// placement used to hit copy_or_link's exists-skip, get counted as
/// organized, and silently vanish from the export.
#[derive(Default)]
struct DestClaims(std::collections::HashSet<String>);

impl DestClaims {
    /// The case-insensitive key one destination is tracked under. ONE spelling,
    /// so [`DestClaims::claim`] and [`DestClaims::is_claimed`] can never
    /// disagree about what has been placed.
    fn key(rel_dir: &str, filename: &str) -> String {
        format!("{}/{}", rel_dir.to_lowercase(), filename.to_lowercase())
    }

    /// Whether this run has already placed `filename` in `rel_dir`.
    fn is_claimed(&self, rel_dir: &str, filename: &str) -> bool {
        self.0.contains(&Self::key(rel_dir, filename))
    }

    fn claim(&mut self, rel_dir: &str, filename: &str) -> String {
        if self.0.insert(Self::key(rel_dir, filename)) {
            return filename.to_string();
        }
        let mut n = 2;
        loop {
            let candidate = match filename.rsplit_once('.') {
                Some((stem, ext)) => format!("{stem}_{n}.{ext}"),
                None => format!("{filename}_{n}"),
            };
            if self.0.insert(Self::key(rel_dir, &candidate)) {
                return candidate;
            }
            n += 1;
        }
    }
}

/// Everything one export run needs to CALIBRATE its lights instead of copying
/// them: one resolved plan per marked frame, the run's options, and the spill
/// directory the pixel phase streams through.
///
/// Built by [`GenerationBatch::resolve`] while a catalog connection is held,
/// then handed to [`organize_files_wbpp`] — which never touches the database.
/// That split is the whole point: resolving is short and needs the catalog,
/// generating is long and must not hold it.
///
/// No lifetime parameter: every field is owned, so the batch outlives the
/// connection it was resolved from (a borrowed spec would pin that connection
/// for the entire pixel phase, which is exactly what this avoids).
#[cfg(feature = "render")]
pub struct GenerationBatch {
    /// frame_id → its resolved plan.
    specs: std::collections::HashMap<i64, crate::export::GenerationSpec>,
    /// frame_id → why it has no plan. Recorded at resolve time so the ONE
    /// warning the operator sees for that frame carries the real reason
    /// instead of a generic "not generated".
    skipped: std::collections::HashMap<i64, String>,
    opts: crate::export::CalibratedLightOptions,
    scratch_dir: PathBuf,
    /// One hot-pixel outcome per master dark, for the whole run: the answer
    /// depends on the dark alone and costs a full plane read to measure, so a
    /// set sharing one dark pays that once — refusals included, which is what
    /// keeps a degenerate dark to ONE warning per run instead of one per frame.
    /// Lives here rather than in `organize_files_wbpp` so that (ungated)
    /// function never names a `render`-only type.
    hot_maps: std::collections::HashMap<
        PathBuf,
        std::sync::Arc<crate::calibration_library::cosmetic::HotPixelMapOutcome>,
    >,
}

/// A build with no pixel pipeline can never generate anything, so the batch is
/// uninhabited there: `organize_files_wbpp` keeps ONE signature across both
/// configurations, and the headless build proves at compile time that its
/// generation arm is unreachable (`match *batch {}`) instead of carrying dead
/// runtime code.
#[cfg(not(feature = "render"))]
pub enum GenerationBatch {}

#[cfg(feature = "render")]
impl GenerationBatch {
    /// Resolve a plan for every light the calibrated-lights transform marked.
    ///
    /// Walks the same [`compute_wbpp_placements`] the organizer will walk, so a
    /// marked frame either gets a spec or gets its failure recorded — there is
    /// no third outcome and no placement the executor can meet unprepared. A
    /// per-frame resolve failure is never fatal: the rest of the export runs
    /// and that one frame is reported.
    pub fn resolve(
        conn: &rusqlite::Connection,
        data: &ExportData,
        opts: crate::export::CalibratedLightOptions,
        scratch_dir: PathBuf,
    ) -> Self {
        let mut specs = std::collections::HashMap::new();
        let mut skipped = std::collections::HashMap::new();
        // ONE memo for the whole batch: resolving a light's flat-norm divisor
        // can read the master flat's entire plane, and a frame set's lights
        // share one flat. See `export::DivisorCache` — it is valid only for the
        // single `opts` this batch resolves under, which is exactly its scope.
        let mut divisors = crate::export::DivisorCache::new();
        for placement in compute_wbpp_placements(data) {
            let PlacementSource::CalibrateLight { frame_id, .. } = placement.source else {
                continue;
            };
            match crate::export::resolve_generation_cached(
                conn,
                frame_id,
                &opts,
                &scratch_dir,
                &mut divisors,
            ) {
                Ok(spec) => {
                    specs.insert(frame_id, spec);
                }
                Err(e) => {
                    tracing::warn!(
                        frame_id,
                        file = %placement.filename,
                        error = %e,
                        "cannot calibrate this light — it will be skipped"
                    );
                    skipped.insert(frame_id, format!("{e:#}"));
                }
            }
        }
        tracing::info!(
            resolved = specs.len(),
            skipped = skipped.len(),
            flat_divisors = divisors.len(),
            "calibrated-light generation planned"
        );
        Self {
            specs,
            skipped,
            opts,
            scratch_dir,
            hot_maps: std::collections::HashMap::new(),
        }
    }
}

/// Generate one placement: calibrate `frame_id` from its plan and write it to
/// `dest`. The write is atomic (temp + rename), so an existing output is
/// REPLACED — there is no exists-skip on a generated file, which would
/// otherwise leave a stale artifact from an earlier run in the export.
///
/// Returns the frame's non-fatal notes (today: a refused hot-pixel map, once
/// per dark for the whole batch) for the caller to fold into
/// [`OrganizeResult::warnings`] — a successful generation can still have
/// something the operator needs to read.
#[cfg(feature = "render")]
fn generate_one(
    batch: &mut GenerationBatch,
    frame_id: i64,
    debayer: bool,
    dest: &Path,
    cancel_flag: &std::sync::atomic::AtomicBool,
) -> Result<Vec<String>> {
    // Field-level destructuring: `specs` is read while `hot_maps` is written,
    // which a whole-struct borrow would not allow.
    let GenerationBatch {
        specs,
        skipped,
        opts,
        scratch_dir,
        hot_maps,
    } = batch;
    let Some(spec) = specs.get(&frame_id) else {
        match skipped.get(&frame_id) {
            Some(reason) => anyhow::bail!("{reason}"),
            None => anyhow::bail!("no calibration plan was resolved for this frame"),
        }
    };
    if spec.debayer != debayer {
        // The name is already claimed; the content is about to be written. They
        // are decided from the same column by the same parser, so a mismatch is
        // a bug worth a log line rather than a silent `_d` disagreement.
        tracing::warn!(
            frame_id,
            named_debayer = debayer,
            resolved_debayer = spec.debayer,
            "debayer decision disagrees with the placed filename"
        );
    }
    let generated =
        crate::export::execute_generation(spec, dest, scratch_dir, opts, hot_maps, cancel_flag)?;
    tracing::debug!(
        frame_id,
        dest = %dest.display(),
        calstat = %generated.calstat,
        debayered = generated.debayered,
        hot_pixels_replaced = generated.hot_pixels_replaced,
        "calibrated light written into the export"
    );
    Ok(generated.warnings)
}

/// Headless stub: [`GenerationBatch`] is uninhabited without the `render`
/// feature, so reaching here is impossible and the compiler knows it.
#[cfg(not(feature = "render"))]
fn generate_one(
    batch: &mut GenerationBatch,
    _frame_id: i64,
    _debayer: bool,
    _dest: &Path,
    _cancel_flag: &std::sync::atomic::AtomicBool,
) -> Result<Vec<String>> {
    match *batch {}
}

/// Organize files for PixInsight WBPP export
///
/// Creates a nested folder structure where parent calibrates child,
/// matching WBPP's "Grouping Keywords with Pre" feature. The hierarchy itself is
/// computed by the pure [`compute_wbpp_placements`] (shared with the Transfers
/// send path); this function only prepends the frame-set root and does the
/// copy/symlink + progress I/O.
///
/// `generation` is the calibrated-lights mode's plan (see [`GenerationBatch`]).
/// `None` — every other mode, and every caller that only copies — means a
/// placement marked for calibration is reported as a warning instead of being
/// silently copied raw under a `c_*` name.
pub fn organize_files_wbpp(
    output_dir: &Path,
    data: &ExportData,
    use_symlinks: bool,
    _config: &WbppExportConfig,
    emitter: Option<&dyn ProgressEmitter>,
    frame_set_id: i64,
    cancel_flag: &std::sync::atomic::AtomicBool,
    generation: Option<&mut GenerationBatch>,
) -> Result<OrganizeResult> {
    let span = tracing::info_span!("export", frame_set_id);
    let _g = span.enter();

    let mut files_organized = 0;
    let mut warnings = Vec::new();

    // Create parent directory named after the frame set — the export layout's top
    // level (the send path omits this; the receiver supplies its own).
    let object_dir = output_dir.join(sanitize_display_folder_name(&data.frame_set_name));

    // `count_total_files` is the historical progress denominator; it agrees with
    // `compute_wbpp_placements().len()` (both dedup by set_id under the same
    // conditions) — pinned by `placement_count_matches_progress_total`.
    let total_files = count_total_files(data);
    let placements = compute_wbpp_placements(data);
    let mut last_emit = Instant::now();

    // Helper closure to emit progress. `phase` distinguishes a copied placement
    // from a generated one — same counter, same denominator, different work.
    //
    // Throttled to every 100ms, because a copy export can place thousands of
    // files a second. `force` opts out for the generation path: calibrating one
    // light takes seconds to minutes, so its events cannot flood anything, and
    // WITHOUT the opt-out the announcement of the frame about to be calibrated
    // would be swallowed by the window the previous frame's success emit just
    // opened — leaving the panel showing the previous filename for the whole
    // calibration.
    let mut emit_progress = |current: usize, filename: Option<&str>, phase: &str, force: bool| {
        let now = Instant::now();
        if force || now.duration_since(last_emit).as_millis() >= 100 || current == total_files {
            if let Some(e) = emitter {
                let percent = if total_files > 0 {
                    (current as f64 / total_files as f64) * 100.0
                } else {
                    0.0
                };
                emit_event(
                    e,
                    "export-progress",
                    &ExportProgressEvent {
                        frame_set_id,
                        current,
                        total: total_files,
                        percent,
                        current_file: filename.map(|s| s.to_string()),
                        phase: phase.to_string(),
                    },
                );
            }
            last_emit = now;
        }
    };

    let mut generation = generation;
    let mut claims = DestClaims::default();
    for placement in &placements {
        if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        // Rebuild the native destination directory from the forward-slash rel_dir.
        let mut dest_dir = object_dir.clone();
        for comp in placement.rel_dir.split('/') {
            dest_dir.push(comp);
        }
        fs::create_dir_all(&dest_dir)?;
        let filename = claims.claim(&placement.rel_dir, &placement.filename);
        let dest = dest_dir.join(&filename);
        match placement.source {
            PlacementSource::Copy => {
                match copy_or_link(&placement.file_path, &dest, use_symlinks) {
                    Ok(_) => {
                        files_organized += 1;
                        emit_progress(files_organized as usize, Some(&filename), "copying", false);
                    }
                    Err(e) => warnings.push(format!("Failed to copy {}: {}", filename, e)),
                }
            }
            PlacementSource::CalibrateLight { frame_id, debayer } => {
                // Announce the frame BEFORE the work, unlike a copy: calibrating
                // one light takes seconds to minutes, and a bar that only moves
                // on completion looks frozen for the whole of it. The count is
                // still what has actually landed.
                emit_progress(files_organized as usize, Some(&filename), "calibrating", true);
                let outcome = match generation.as_deref_mut() {
                    Some(batch) => generate_one(batch, frame_id, debayer, &dest, cancel_flag),
                    None => Err(anyhow::anyhow!(
                        "this export was not prepared to calibrate lights"
                    )),
                };
                match outcome {
                    Ok(notes) => {
                        files_organized += 1;
                        warnings.extend(notes);
                        remove_stale_sibling(
                            &dest_dir,
                            &placement.file_path,
                            &filename,
                            debayer,
                            |name| claims.is_claimed(&placement.rel_dir, name),
                            &mut warnings,
                        );
                        emit_progress(
                            files_organized as usize,
                            Some(&filename),
                            "calibrating",
                            true,
                        );
                    }
                    Err(e) => {
                        // A cancel surfaces here as a per-frame error; the loop
                        // is about to end anyway, so it is not a warning the
                        // operator needs to read.
                        if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }
                        tracing::warn!(
                            frame_id,
                            dest = %dest.display(),
                            error = %format!("{e:#}"),
                            "calibrated light generation failed — frame skipped"
                        );
                        warnings.push(format!("Failed to calibrate {}: {:#}", filename, e));
                        // Review fix #5: sweep the opposite-toggle sibling on
                        // failure too. `dest` itself is never written here —
                        // `generate_one`'s output is atomic (temp + rename) —
                        // so a failed regeneration would otherwise leave the
                        // PREVIOUS run's opposite-toggle output in place, and
                        // WBPP would ingest that stale artifact for a light
                        // this run just reported as failed. A frame that
                        // fails to regenerate should leave no artifact
                        // behind, not a stale one. `claims.claim` above
                        // already reserved `filename` regardless of outcome,
                        // so `is_claimed` still refuses to delete a file this
                        // same run placed.
                        remove_stale_sibling(
                            &dest_dir,
                            &placement.file_path,
                            &filename,
                            debayer,
                            |name| claims.is_claimed(&placement.rel_dir, name),
                            &mut warnings,
                        );
                    }
                }
            }
        }
    }

    Ok(OrganizeResult {
        files_organized,
        warnings,
    })
}

/// Delete the output the OTHER debayer setting would have written for the same
/// source light, if an earlier export into this same folder left one behind.
///
/// The toggle decides the NAME (`c_x.fits` vs `c_x_d.fits`), so re-exporting a
/// frame set with it flipped used to leave both files side by side and WBPP
/// ingested the frame twice. Only that one sibling name, only in the directory
/// just written, and only when the placed name is exactly the one this source
/// maps to — a collision-suffixed placement (`c_x_2.fits`) belongs to a
/// different frame's stem and must not have anything deleted on its behalf.
///
/// Both names come from [`calibrated_output_filename`] over the SAME source
/// spelling, so the pair can never describe two different frames. A failure to
/// remove is reported, never fatal: whatever this frame's own generation did
/// or did not produce is unaffected either way.
///
/// Called on BOTH generation outcomes (review fix #5) — a successful write
/// (the export's own output is already written) and a failed one (`dest`
/// itself was never written, since [`generate_one`]'s output is atomic; a
/// failure must still leave no artifact for this frame, not a stale one from
/// an earlier run with the toggle flipped).
///
/// `already_placed` is the run's own claim ledger: a source named `x_d.fits`
/// sitting beside `x.fits` maps to the same output name as `x.fits` debayered,
/// and this sweep must never delete a file THIS export just wrote — the
/// caller claims a placement's name before it knows whether generation will
/// succeed, so the guard holds on both arms.
fn remove_stale_sibling(
    dest_dir: &Path,
    source_path: &str,
    placed_filename: &str,
    debayer: bool,
    already_placed: impl Fn(&str) -> bool,
    warnings: &mut Vec<String>,
) {
    let Some(source_name) = Path::new(source_path).file_name().and_then(|n| n.to_str()) else {
        return;
    };
    if calibrated_output_filename(source_name, debayer) != placed_filename {
        return;
    }
    let sibling_name = calibrated_output_filename(source_name, !debayer);
    if already_placed(&sibling_name) {
        return;
    }
    let sibling = dest_dir.join(sibling_name);
    if !sibling.exists() {
        return;
    }
    match fs::remove_file(&sibling) {
        Ok(()) => tracing::info!(
            path = %sibling.display(),
            "stale calibrated sibling removed"
        ),
        Err(e) => {
            tracing::warn!(
                path = %sibling.display(),
                error = %e,
                "stale calibrated sibling could not be removed"
            );
            warnings.push(format!(
                "Could not remove {}, left by an earlier export with the other debayer setting: {}",
                sibling.display(),
                e
            ));
        }
    }
}

/// Copy file or create symlink
fn copy_or_link(source: &str, dest: &PathBuf, use_symlinks: bool) -> Result<()> {
    // Skip if destination already exists
    if dest.exists() {
        return Ok(());
    }

    if use_symlinks {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(source, dest)
                .with_context(|| format!("Failed to symlink {} -> {:?}", source, dest))?;
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(source, dest)
                .with_context(|| format!("Failed to symlink {} -> {:?}", source, dest))?;
        }
    } else {
        fs::copy(source, dest)
            .with_context(|| format!("Failed to copy {} -> {:?}", source, dest))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::models::{
        CalibrationSummary, CameraType, ExportFrame, ExportGroup, MasterCreationPlan,
    };
    use std::collections::BTreeSet;

    fn frame(id: i64, filename: &str, instrume: &str) -> ExportFrame {
        ExportFrame {
            frame_id: id,
            file_id: id,
            file_path: format!("/src/{filename}"),
            filename: filename.to_string(),
            exptime: Some(60.0),
            filter: None,
            ccd_temp: None,
            gain: None,
            offset: None,
            binning: None,
            date_obs: Some(format!("2026-01-0{}T00:00:00", (id % 9) + 1)),
            focallen: None,
            xpixsz: None,
            bayerpat: None,
            instrume: Some(instrume.to_string()),
            debayer_calibrated: None,
        }
    }

    fn set_info(
        set_id: i64,
        imagetyp: &str,
        frames: Vec<ExportFrame>,
        dark_flat: Option<Box<CalibrationSetInfo>>,
        dark: Option<Box<CalibrationSetInfo>>,
        bias: Option<Box<CalibrationSetInfo>>,
    ) -> CalibrationSetInfo {
        let frame_count = frames.len() as i32;
        CalibrationSetInfo {
            set_id,
            imagetyp: imagetyp.to_string(),
            frames,
            frame_count,
            dark_flat,
            dark,
            bias,
            match_score: None,
            warnings: Vec::new(),
        }
    }

    fn subgroup(
        lights: Vec<ExportFrame>,
        flat: Option<CalibrationSetInfo>,
        dark: Option<CalibrationSetInfo>,
        bias: Option<CalibrationSetInfo>,
    ) -> CalibrationSubgroup {
        CalibrationSubgroup {
            subgroup_key: "k".to_string(),
            display_name: "Default".to_string(),
            frames: lights,
            flat,
            dark,
            bias,
            warnings: Vec::new(),
        }
    }

    fn export_data(name: &str, subgroups: Vec<CalibrationSubgroup>) -> ExportData {
        ExportData {
            frame_set_id: 1,
            frame_set_name: name.to_string(),
            object_name: None,
            groups: vec![ExportGroup {
                group_key: "g".to_string(),
                filter: None,
                camera_type: CameraType::Mono,
                display_name: "g".to_string(),
                subgroups,
                total_frames: 0,
                total_exposure: 0.0,
                warnings: Vec::new(),
            }],
            master_plan: MasterCreationPlan {
                masters: Vec::new(),
                master_paths: std::collections::HashMap::new(),
            },
            filters: Vec::new(),
            calibration_summary: CalibrationSummary {
                flat_count: 0,
                dark_count: 0,
                bias_count: 0,
                dark_flat_count: 0,
                flats_complete: true,
                darks_complete: true,
                bias_complete: true,
                warnings: Vec::new(),
            },
            total_light_frames: 0,
            total_exposure_seconds: 0.0,
        }
    }

    /// Full WBPP hierarchy: a light + a Flat (with its own DarkFlat) + a Dark
    /// (with its own Bias). Each selected frame lands at its documented level, and
    /// the frame-set name is NOT part of `rel_dir`.
    #[test]
    fn wbpp_placements_full_hierarchy() {
        let bias_of_dark = set_info(30, "Bias", vec![frame(300, "bias1.fits", "ASI")], None, None, None);
        let dark = set_info(
            20,
            "Dark",
            vec![frame(200, "dark1.fits", "ASI")],
            None,
            None,
            Some(Box::new(bias_of_dark)),
        );
        let darkflat = set_info(
            11,
            "DarkFlat",
            vec![frame(110, "df1.fits", "ASI")],
            None,
            None,
            None,
        );
        let flat = set_info(
            10,
            "Flat",
            vec![frame(100, "flat1.fits", "ASI")],
            Some(Box::new(darkflat)),
            None,
            None,
        );
        let sg = subgroup(
            vec![frame(1, "light1.fits", "ASI")],
            Some(flat),
            Some(dark),
            None,
        );
        let data = export_data("M31 Set", vec![sg]);

        let placements = compute_wbpp_placements(&data);
        let by_id: std::collections::HashMap<i64, String> = placements
            .iter()
            .map(|p| (p.frame_id, p.rel_dir.clone()))
            .collect();

        // camera_asi/BIAS_30/DARKS_20/FLAT_10/lights
        assert_eq!(by_id[&1], "camera_asi/BIAS_30/DARKS_20/FLAT_10/lights");
        // Dark's own bias becomes the outer BIAS level (no top-level bias/flat-bias).
        assert_eq!(by_id[&300], "camera_asi/BIAS_30");
        assert_eq!(by_id[&200], "camera_asi/BIAS_30/DARKS_20");
        assert_eq!(by_id[&110], "camera_asi/BIAS_30/DARKS_20"); // DarkFlat at DARKS level
        assert_eq!(by_id[&100], "camera_asi/BIAS_30/DARKS_20/FLAT_10");
        // No frame-set name anywhere in rel_dir.
        assert!(placements.iter().all(|p| !p.rel_dir.contains("M31")));
    }

    /// Missing calibration levels collapse: a light with ONLY a Flat linked lands
    /// under `camera_.../FLAT_.../lights` — no BIAS or DARKS directory appears.
    #[test]
    fn wbpp_placements_collapse_missing_levels() {
        let flat = set_info(
            10,
            "Flat",
            vec![frame(100, "flat1.fits", "ASI")],
            None,
            None,
            None,
        );
        let sg = subgroup(vec![frame(1, "light1.fits", "ASI")], Some(flat), None, None);
        let data = export_data("Set", vec![sg]);

        let placements = compute_wbpp_placements(&data);
        let by_id: std::collections::HashMap<i64, String> = placements
            .iter()
            .map(|p| (p.frame_id, p.rel_dir.clone()))
            .collect();

        assert_eq!(by_id[&1], "camera_asi/FLAT_10/lights");
        assert_eq!(by_id[&100], "camera_asi/FLAT_10");
        assert!(placements.iter().all(|p| !p.rel_dir.contains("BIAS")));
        assert!(placements.iter().all(|p| !p.rel_dir.contains("DARKS")));
    }

    /// Bare light (no calibration links) lands directly under `camera_.../lights`.
    #[test]
    fn wbpp_placements_lights_only() {
        let sg = subgroup(
            vec![frame(1, "a.fits", "ASI"), frame(2, "b.fits", "ASI")],
            None,
            None,
            None,
        );
        let data = export_data("Set", vec![sg]);
        let placements = compute_wbpp_placements(&data);
        assert_eq!(placements.len(), 2);
        assert!(placements.iter().all(|p| p.rel_dir == "camera_asi/lights"));
    }

    /// The pure placement count must equal the historical progress denominator
    /// `count_total_files` — the invariant `organize_files_wbpp` relies on when it
    /// counts placements against that total.
    #[test]
    fn placement_count_matches_progress_total() {
        let bias_of_dark = set_info(
            30,
            "Bias",
            vec![frame(300, "bias1.fits", "ASI")],
            None,
            None,
            None,
        );
        let dark = set_info(
            20,
            "Dark",
            vec![
                frame(200, "dark1.fits", "ASI"),
                frame(201, "dark2.fits", "ASI"),
            ],
            None,
            None,
            Some(Box::new(bias_of_dark)),
        );
        let darkflat = set_info(
            11,
            "DarkFlat",
            vec![frame(110, "df1.fits", "ASI")],
            None,
            None,
            None,
        );
        let flat = set_info(
            10,
            "Flat",
            vec![frame(100, "flat1.fits", "ASI")],
            Some(Box::new(darkflat)),
            None,
            None,
        );
        let sg = subgroup(
            vec![frame(1, "l1.fits", "ASI"), frame(2, "l2.fits", "ASI")],
            Some(flat),
            Some(dark),
            None,
        );
        let data = export_data("Set", vec![sg]);
        assert_eq!(
            compute_wbpp_placements(&data).len(),
            count_total_files(&data)
        );
    }

    /// Byte-identical layout pin: running the real (I/O) organizer over a small
    /// hierarchy produces exactly the documented WBPP tree under
    /// `<out>/<frame_set_name>/…`. Guards the refactor that routed the copy path
    /// through `compute_wbpp_placements`.
    #[test]
    fn organize_files_wbpp_writes_expected_tree() {
        let src = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();

        // Real source files so `fs::copy` succeeds.
        let mk = |name: &str| {
            let p = src.path().join(name);
            std::fs::write(&p, b"x").unwrap();
            p.to_string_lossy().into_owned()
        };
        let f = |id: i64, name: &str| {
            let mut fr = frame(id, name, "ASI");
            fr.file_path = mk(name);
            fr
        };

        let dark = set_info(20, "Dark", vec![f(200, "dark1.fits")], None, None, None);
        let flat = set_info(10, "Flat", vec![f(100, "flat1.fits")], None, None, None);
        let sg = subgroup(vec![f(1, "light1.fits")], Some(flat), Some(dark), None);
        let data = export_data("My Set", vec![sg]);

        let cancel = std::sync::atomic::AtomicBool::new(false);
        let config = WbppExportConfig::default();
        let result =
            organize_files_wbpp(out.path(), &data, false, &config, None, 1, &cancel, None).unwrap();
        assert_eq!(result.files_organized, 3);
        assert!(result.warnings.is_empty());

        // Collect every regular file created, relative to the output root.
        let mut got: BTreeSet<String> = BTreeSet::new();
        for entry in walkdir(out.path()) {
            if entry.is_file() {
                got.insert(
                    entry
                        .strip_prefix(out.path())
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
        // Lights nest under FLAT (flat present); the Dark set has no top-level
        // bias, so DARKS is the outermost calibration level.
        let expected: BTreeSet<String> = [
            "My Set/camera_asi/DARKS_20/dark1.fits".to_string(),
            "My Set/camera_asi/DARKS_20/FLAT_10/flat1.fits".to_string(),
            "My Set/camera_asi/DARKS_20/FLAT_10/lights/light1.fits".to_string(),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn dest_claims_disambiguates_case_collisions() {
        let mut claims = DestClaims::default();
        assert_eq!(claims.claim("lights", "L_0001.fits"), "L_0001.fits");
        assert_eq!(claims.claim("lights", "l_0001.FITS"), "l_0001_2.FITS");
        assert_eq!(claims.claim("lights", "L_0001.fits"), "L_0001_3.fits");
        // Different directory — no rename.
        assert_eq!(claims.claim("FLAT_1", "L_0001.fits"), "L_0001.fits");
    }

    /// Catalog + on-disk fixture for the generation arm: frame set "My Set",
    /// one `TestCam` session, `lights` LIGHT frames (each a real 8x8 FITS
    /// plane) and ONE master dark set linked to all of them — one dark means
    /// one subgroup, so the placement order below is the order the lights are
    /// given in.
    ///
    /// The dark alternates 300/302 (median 301, MAD 1.0 → threshold 315.8), so
    /// its hot-pixel map is MEASURED and empty. A uniform dark would be REFUSED
    /// instead, which is a warning of its own and would mask the ones these
    /// tests are about.
    ///
    /// Returns the connection and the master dark's path.
    #[cfg(feature = "render")]
    fn seed_generation_fixture(
        src: &Path,
        lights: &[(i64, &str)],
        bayerpat: Option<&str>,
    ) -> (rusqlite::Connection, PathBuf) {
        use rusqlite::params;

        const W: usize = 8;
        const H: usize = 8;
        let write_plane = |path: &Path, fill: &dyn Fn(usize, usize) -> f32| {
            let mut data = vec![0f32; W * H];
            for y in 0..H {
                for x in 0..W {
                    data[y * W + x] = fill(x, y);
                }
            }
            crate::fits_writer::write_fits_f32(path, W, H, 1, &data, &[]).unwrap();
        };

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'My Set')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
             VALUES (1, '2026-07-05T20:00:00Z', '2026-07-05T23:00:00Z')",
            [],
        )
        .unwrap();
        let night_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'TestCam')",
            params![night_id],
        )
        .unwrap();
        let session_id = conn.last_insert_rowid();

        // The master dark set, shared by every light.
        let dark_path = src.join("master_dark.fits");
        write_plane(&dark_path, &|x, y| {
            if (x + y) % 2 == 0 {
                300.0
            } else {
                302.0
            }
        });
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (200, 'Dark', '2026-07-05', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (2, ?1, 'master_dark.fits', 0, '2026-07-05T00:00:00Z', 'FITS')",
            params![dark_path.to_string_lossy()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, is_master) VALUES (20, 2, 'Dark', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (200, 20)",
            [],
        )
        .unwrap();

        for (frame_id, name) in lights {
            let light_path = src.join(name);
            write_plane(&light_path, &|_, _| 1000.0);
            let file_id = frame_id + 1_000;
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
                params![file_id, light_path.to_string_lossy(), name],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp, instrume, object, date_obs, filter,
                                     bayerpat, xbayroff, ybayroff)
                 VALUES (?1, ?2, 'Light', 'TestCam', 'M31', '2026-07-05T20:30:00Z', 'Ha',
                         ?3, ?4, ?4)",
                params![frame_id, file_id, bayerpat, bayerpat.map(|_| 0i64)],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                params![session_id, frame_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO calibration_set_to_frames
                 (source_id, source_type, calibration_set_id, calibration_type, matched_at)
                 VALUES (?1, 'frame', 200, 'Dark', '2026-07-05T00:00:00Z')",
                params![frame_id],
            )
            .unwrap();
        }
        (conn, dark_path)
    }

    /// Collect + transform + resolve for the calibrated-lights mode, the three
    /// steps every generation test does before it can call the organizer.
    #[cfg(feature = "render")]
    fn resolve_calibrated_batch(
        conn: &rusqlite::Connection,
        scratch: &Path,
        debayer: bool,
    ) -> (ExportData, GenerationBatch) {
        use crate::export::models::CalibratedLightOptions;
        let mut data = crate::export::collect_export_data(conn, 1).unwrap();
        let opts = CalibratedLightOptions {
            debayer_osc: debayer,
            ..CalibratedLightOptions::default()
        };
        crate::export::apply_export_mode(
            conn,
            &mut data,
            crate::export::models::ExportMode::CalibratedLights,
            Some(&opts),
        )
        .unwrap();
        let batch = GenerationBatch::resolve(conn, &data, opts, scratch.to_path_buf());
        (data, batch)
    }

    /// Every regular file under `root`, forward-slashed and relative to it.
    #[cfg(feature = "render")]
    fn files_under(root: &Path) -> Vec<String> {
        let mut v: Vec<String> = walkdir(root)
            .into_iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        v.sort();
        v
    }

    /// End-to-end for the generation path: a real (tiny) light + master dark on
    /// disk, the calibrated-lights transform, a resolved batch, and the
    /// organizer — the output must be a CALIBRATED file written under its `c_*`
    /// name, counted like any other placement, with no calibration folder in the
    /// tree. Debayer is off so the assertion is about generation, not mosaics.
    #[cfg(feature = "render")]
    #[test]
    fn organize_generates_calibrated_lights() {
        let src = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();

        let (conn, _dark) = seed_generation_fixture(src.path(), &[(10, "light_10.fits")], None);
        let (data, mut batch) = resolve_calibrated_batch(&conn, scratch.path(), false);
        drop(conn); // the pixel phase holds no catalog connection

        let cancel = std::sync::atomic::AtomicBool::new(false);
        let config = WbppExportConfig::default();
        let result = organize_files_wbpp(
            out.path(),
            &data,
            false,
            &config,
            None,
            1,
            &cancel,
            Some(&mut batch),
        )
        .unwrap();
        assert_eq!(result.files_organized, 1, "warnings: {:?}", result.warnings);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);

        let written = out
            .path()
            .join("My Set/camera_testcam/lights/c_light_10.fits");
        assert!(written.exists(), "calibrated output missing at {written:?}");
        let header = crate::fits_parser::FitsHeader::from_path(&written).unwrap();
        assert_eq!(header.get_str("CALSTAT").as_deref(), Some("BD"));
        // The dark's map was measured and empty — the pass ran, replacing none.
        assert_eq!(header.get_i32("ATH_CHPX"), Some(0));

        // Calibrated, not copied: the light is 1000 everywhere and the dark
        // alternates 300/302, so the first row reads 700, 698 — a copy would
        // read 1000, 1000.
        let mut plane =
            crate::integration::banded::BandSource::open(&[written.clone()], scratch.path())
                .unwrap();
        let mut bufs = vec![Vec::new()];
        plane.read_band(0, 1, &mut bufs).unwrap();
        assert!((bufs[0][0] - 700.0).abs() < 1e-3, "got {}", bufs[0][0]);
        assert!((bufs[0][1] - 698.0).abs() < 1e-3, "got {}", bufs[0][1]);

        // The raw light was NOT placed, and no calibration folder exists.
        assert_eq!(
            files_under(out.path()),
            vec!["My Set/camera_testcam/lights/c_light_10.fits"]
        );
    }

    /// Spec §11 failure isolation: one light that cannot be generated costs the
    /// export that ONE frame and nothing else. The middle light's source file
    /// is deleted after the batch resolves (resolution reads the catalog and
    /// the header; the pixel phase is what fails), so the run must place the
    /// other two, report exactly one warning naming the casualty, and return
    /// `Ok` — an export that dies on frame 2 of 200 would be the real defect.
    #[cfg(feature = "render")]
    #[test]
    fn generation_failure_isolates_one_frame() {
        let src = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();

        let lights = [
            (10i64, "light_10.fits"),
            (11, "light_11.fits"),
            (12, "light_12.fits"),
        ];
        let (conn, _dark) = seed_generation_fixture(src.path(), &lights, None);
        let (data, mut batch) = resolve_calibrated_batch(&conn, scratch.path(), false);
        drop(conn);

        // One subgroup, so the placement order is the seeding order — this is
        // what makes "the MIDDLE one fails" a statement about this batch.
        let placed: Vec<String> = compute_wbpp_placements(&data)
            .into_iter()
            .map(|p| p.filename)
            .collect();
        assert_eq!(
            placed,
            vec!["c_light_10.fits", "c_light_11.fits", "c_light_12.fits"]
        );

        std::fs::remove_file(src.path().join("light_11.fits")).unwrap();

        let cancel = std::sync::atomic::AtomicBool::new(false);
        let config = WbppExportConfig::default();
        let result = organize_files_wbpp(
            out.path(),
            &data,
            false,
            &config,
            None,
            1,
            &cancel,
            Some(&mut batch),
        )
        .expect("one unusable frame must not fail the export");

        assert_eq!(result.files_organized, 2);
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
        assert!(
            result.warnings[0].contains("c_light_11.fits"),
            "the warning must name the frame that was skipped: {}",
            result.warnings[0]
        );
        assert_eq!(
            files_under(out.path()),
            vec![
                "My Set/camera_testcam/lights/c_light_10.fits",
                "My Set/camera_testcam/lights/c_light_12.fits",
            ]
        );
    }

    /// A cancel raised while the batch is running stops it: the frames after
    /// the cancelled one are never generated, and the run still returns `Ok`
    /// (a cancel is not a failure) with no warning about the frame it dropped.
    ///
    /// The flag is raised from the progress emitter — the organizer announces
    /// each light BEFORE calibrating it, so raising the flag on the second
    /// announcement lands inside frame 2's own work, deterministically, with
    /// no test-only hook in the generator and no thread racing the loop.
    #[cfg(feature = "render")]
    #[test]
    fn cancel_mid_batch_stops_the_remaining_frames() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};

        /// Raises `cancel` the first time a DIFFERENT file is announced, i.e.
        /// when the organizer starts the second light.
        struct CancelOnSecondFrame {
            cancel: Arc<AtomicBool>,
            first: Mutex<Option<String>>,
        }
        impl crate::events::ProgressEmitter for CancelOnSecondFrame {
            fn emit_json(&self, _event: &str, payload: serde_json::Value) {
                let Some(file) = payload.get("currentFile").and_then(|v| v.as_str()) else {
                    return;
                };
                let mut first = self.first.lock().unwrap();
                match first.as_deref() {
                    None => *first = Some(file.to_string()),
                    Some(seen) if seen != file => self.cancel.store(true, Ordering::Relaxed),
                    _ => {}
                }
            }
        }

        let src = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();

        let lights = [
            (10i64, "light_10.fits"),
            (11, "light_11.fits"),
            (12, "light_12.fits"),
        ];
        let (conn, _dark) = seed_generation_fixture(src.path(), &lights, None);
        let (data, mut batch) = resolve_calibrated_batch(&conn, scratch.path(), false);
        drop(conn);

        let cancel = Arc::new(AtomicBool::new(false));
        let emitter = CancelOnSecondFrame {
            cancel: Arc::clone(&cancel),
            first: Mutex::new(None),
        };
        let config = WbppExportConfig::default();
        let result = organize_files_wbpp(
            out.path(),
            &data,
            false,
            &config,
            Some(&emitter),
            1,
            &cancel,
            Some(&mut batch),
        )
        .expect("a cancel is not an export failure");

        assert!(cancel.load(Ordering::Relaxed), "the emitter never fired");
        assert_eq!(
            result.files_organized, 1,
            "only the frame that finished before the cancel counts"
        );
        assert!(
            result.warnings.is_empty(),
            "a cancelled frame is not a warning the operator needs: {:?}",
            result.warnings
        );
        assert_eq!(
            files_under(out.path()),
            vec!["My Set/camera_testcam/lights/c_light_10.fits"],
            "the third light must never be generated"
        );
    }

    /// Flipping the debayer toggle changes the output NAME, so a re-export into
    /// the same folder used to leave BOTH `c_x_d.fits` and `c_x.fits` there and
    /// WBPP ingested the frame twice. The second run must clear the sibling the
    /// first one wrote.
    #[cfg(feature = "render")]
    #[test]
    fn flipping_debayer_removes_the_stale_sibling() {
        let src = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();

        // An OSC light, so the debayer toggle actually changes the name.
        let (conn, _dark) =
            seed_generation_fixture(src.path(), &[(10, "light_10.fits")], Some("RGGB"));
        let config = WbppExportConfig::default();
        let cancel = std::sync::atomic::AtomicBool::new(false);

        for debayer in [true, false] {
            let (data, mut batch) = resolve_calibrated_batch(&conn, scratch.path(), debayer);
            let result = organize_files_wbpp(
                out.path(),
                &data,
                false,
                &config,
                None,
                1,
                &cancel,
                Some(&mut batch),
            )
            .unwrap();
            assert_eq!(result.files_organized, 1, "{:?}", result.warnings);
            assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        }

        assert_eq!(
            files_under(out.path()),
            vec!["My Set/camera_testcam/lights/c_light_10.fits"],
            "the debayered output of the first run must not survive the second"
        );
    }

    /// Review fix #5: a failed regeneration must not leave the PREVIOUS run's
    /// opposite-toggle output behind. First run (debayer on) writes
    /// `c_light_10_d.fits`; the source is then removed so the second run
    /// (debayer off) fails to generate `light_10` at all — and the stale
    /// `_d` sibling from the first run must be swept even though this run's
    /// own `c_light_10.fits` was never written.
    #[cfg(feature = "render")]
    #[test]
    fn failed_regeneration_still_sweeps_the_stale_sibling() {
        let src = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();

        // An OSC light, so the debayer toggle actually changes the name.
        let (conn, _dark) =
            seed_generation_fixture(src.path(), &[(10, "light_10.fits")], Some("RGGB"));
        let config = WbppExportConfig::default();
        let cancel = std::sync::atomic::AtomicBool::new(false);

        // First run: debayer ON, succeeds, writes c_light_10_d.fits.
        let (data, mut batch) = resolve_calibrated_batch(&conn, scratch.path(), true);
        let result = organize_files_wbpp(
            out.path(),
            &data,
            false,
            &config,
            None,
            1,
            &cancel,
            Some(&mut batch),
        )
        .unwrap();
        assert_eq!(result.files_organized, 1, "{:?}", result.warnings);
        assert_eq!(
            files_under(out.path()),
            vec!["My Set/camera_testcam/lights/c_light_10_d.fits"]
        );

        // The source vanishes (archived/moved), so the second run's pixel
        // phase fails for this frame.
        std::fs::remove_file(src.path().join("light_10.fits")).unwrap();

        // Second run: debayer OFF, and the frame fails to regenerate.
        let (data, mut batch) = resolve_calibrated_batch(&conn, scratch.path(), false);
        let result = organize_files_wbpp(
            out.path(),
            &data,
            false,
            &config,
            None,
            1,
            &cancel,
            Some(&mut batch),
        )
        .unwrap();
        assert_eq!(result.files_organized, 0, "the only frame failed");
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
        assert!(
            result.warnings[0].contains("Failed to calibrate"),
            "{:?}",
            result.warnings
        );
        assert!(
            files_under(out.path()).is_empty(),
            "the failed frame must leave no artifact — neither its own \
             (never written) nor the earlier run's opposite-toggle output: {:?}",
            files_under(out.path())
        );
    }

    /// The sibling sweep must never delete a file THIS export placed. A source
    /// literally named `x_d.fits` produces `c_x_d.fits` — the same name the
    /// debayered output of `x.fits` would take — so with both in one frame set
    /// the sweep for `c_x.fits` aims straight at the other frame's output.
    ///
    /// `x_d.fits` is placed FIRST, so an unguarded sweep would delete it with
    /// nothing left in the run to write it again: one light silently missing
    /// from the export.
    #[cfg(feature = "render")]
    #[test]
    fn sibling_sweep_spares_a_file_this_run_placed() {
        let src = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();

        let (conn, _dark) =
            seed_generation_fixture(src.path(), &[(10, "x_d.fits"), (11, "x.fits")], None);
        let (data, mut batch) = resolve_calibrated_batch(&conn, scratch.path(), false);
        drop(conn);

        let placed: Vec<String> = compute_wbpp_placements(&data)
            .into_iter()
            .map(|p| p.filename)
            .collect();
        assert_eq!(
            placed,
            vec!["c_x_d.fits", "c_x.fits"],
            "the collision only bites when the sibling is placed first"
        );

        let cancel = std::sync::atomic::AtomicBool::new(false);
        let config = WbppExportConfig::default();
        let result = organize_files_wbpp(
            out.path(),
            &data,
            false,
            &config,
            None,
            1,
            &cancel,
            Some(&mut batch),
        )
        .unwrap();
        assert_eq!(result.files_organized, 2, "{:?}", result.warnings);
        assert_eq!(
            files_under(out.path()),
            vec![
                "My Set/camera_testcam/lights/c_x.fits",
                "My Set/camera_testcam/lights/c_x_d.fits",
            ],
            "both lights must survive their own export"
        );
    }

    /// Minimal recursive file walker for the layout pin (avoids a walkdir dep).
    fn walkdir(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push(p);
                }
            }
        }
        out
    }
}
