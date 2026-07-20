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
    sanitize_display_folder_name, sanitize_folder_name, CalibrationSetInfo, CalibrationSubgroup,
    ExportData, ExportProgressEvent, WbppExportConfig,
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
        });
    }
}

/// Organize files for PixInsight WBPP export
///
/// Creates a nested folder structure where parent calibrates child,
/// matching WBPP's "Grouping Keywords with Pre" feature. The hierarchy itself is
/// computed by the pure [`compute_wbpp_placements`] (shared with the Transfers
/// send path); this function only prepends the frame-set root and does the
/// copy/symlink + progress I/O.
pub fn organize_files_wbpp(
    output_dir: &Path,
    data: &ExportData,
    use_symlinks: bool,
    _config: &WbppExportConfig,
    emitter: Option<&dyn ProgressEmitter>,
    frame_set_id: i64,
    cancel_flag: &std::sync::atomic::AtomicBool,
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

    // Helper closure to emit progress (throttled to every 100ms)
    let mut emit_progress = |current: usize, filename: Option<&str>| {
        let now = Instant::now();
        if now.duration_since(last_emit).as_millis() >= 100 || current == total_files {
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
                        phase: "copying".to_string(),
                    },
                );
            }
            last_emit = now;
        }
    };

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
        let dest = dest_dir.join(&placement.filename);
        match copy_or_link(&placement.file_path, &dest, use_symlinks) {
            Ok(_) => {
                files_organized += 1;
                emit_progress(files_organized as usize, Some(&placement.filename));
            }
            Err(e) => warnings.push(format!("Failed to copy {}: {}", placement.filename, e)),
        }
    }

    Ok(OrganizeResult {
        files_organized,
        warnings,
    })
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
        let darkflat = set_info(11, "DarkFlat", vec![frame(110, "df1.fits", "ASI")], None, None, None);
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
        let by_id: std::collections::HashMap<i64, String> =
            placements.iter().map(|p| (p.frame_id, p.rel_dir.clone())).collect();

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
        let flat = set_info(10, "Flat", vec![frame(100, "flat1.fits", "ASI")], None, None, None);
        let sg = subgroup(vec![frame(1, "light1.fits", "ASI")], Some(flat), None, None);
        let data = export_data("Set", vec![sg]);

        let placements = compute_wbpp_placements(&data);
        let by_id: std::collections::HashMap<i64, String> =
            placements.iter().map(|p| (p.frame_id, p.rel_dir.clone())).collect();

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
        let bias_of_dark = set_info(30, "Bias", vec![frame(300, "bias1.fits", "ASI")], None, None, None);
        let dark = set_info(
            20,
            "Dark",
            vec![frame(200, "dark1.fits", "ASI"), frame(201, "dark2.fits", "ASI")],
            None,
            None,
            Some(Box::new(bias_of_dark)),
        );
        let darkflat = set_info(11, "DarkFlat", vec![frame(110, "df1.fits", "ASI")], None, None, None);
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
        assert_eq!(compute_wbpp_placements(&data).len(), count_total_files(&data));
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
            organize_files_wbpp(out.path(), &data, false, &config, None, 1, &cancel).unwrap();
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

    /// Minimal recursive file walker for the layout pin (avoids a walkdir dep).
    fn walkdir(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
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
