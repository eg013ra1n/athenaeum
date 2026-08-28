//! Data collector for export operations
//!
//! Collects light frames from a frame set and their linked calibrations
//! to prepare data for export.

use crate::db::light_calibrations::get_light_calibration_for_frame;
use crate::export::models::{
    CalibrationDetail, CalibrationSetInfo, CalibrationSubgroup, CalibrationSummary, CameraType,
    DetailedWarning, ExportCalibrationSet, ExportData, ExportFileCounts, ExportFrame, ExportGroup,
    ExportMode, ExportSummary, ExposureGroup, FilterExportGroup, FilterGroupSummary, FolderNode,
    FolderNodeType, FolderPreview, FrameDetail, MasterCreationPlan, MasterInfo, WarningSeverity,
    WarningType, WbppExportConfig,
};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Collect all export data for a frame set
///
/// This traverses the frame set hierarchy to get all light frames,
/// groups them by filter and camera type, and retrieves their calibration links.
pub fn collect_export_data(conn: &Connection, frame_set_id: i64) -> Result<ExportData> {
    tracing::debug!(frame_set_id, "collecting export data");

    // Get frame set info
    let (frame_set_name, object_name) = get_frame_set_info(conn, frame_set_id)?;
    tracing::debug!(frame_set_id, name = %frame_set_name, object = ?object_name, "frame set info loaded");

    // Get all light frames from the frame set
    let light_frames = get_light_frames_for_frame_set(conn, frame_set_id)?;
    tracing::debug!(frame_set_id, count = light_frames.len(), "light frames loaded");

    // =========================================================================
    // Phase 3: Build new export groups with subgroups
    // =========================================================================
    let groups = build_export_groups(conn, &light_frames)?;
    let master_plan = build_master_creation_plan(conn, &groups)?;

    tracing::debug!(
        frame_set_id,
        groups = groups.len(),
        masters = master_plan.masters.len(),
        "export groups built"
    );

    // =========================================================================
    // Legacy: Build filter groups for backwards compatibility
    // =========================================================================
    let mut filter_groups: HashMap<Option<String>, Vec<ExportFrame>> = HashMap::new();
    for frame in &light_frames {
        filter_groups
            .entry(frame.filter.clone())
            .or_default()
            .push(frame.clone());
    }

    // Build filter export groups with calibrations (legacy format)
    let mut filters = Vec::new();
    let mut total_flat_count = 0;
    let mut total_dark_count = 0;
    let mut total_bias_count = 0;
    let mut total_dark_flat_count = 0;
    let mut all_warnings = Vec::new();
    let mut flats_complete = true;
    let mut darks_complete = true;
    let mut bias_complete = true;

    for (filter, frames) in filter_groups {
        // For each filter group, collect calibrations from the first frame
        // (calibrations are typically shared within a filter group)
        let (flat_sets, dark_sets, bias_sets) = if let Some(first_frame) = frames.first() {
            collect_calibrations_for_frame(conn, first_frame.frame_id)?
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };

        // Update summary counts
        for flat in &flat_sets {
            total_flat_count += flat.frames.len() as i32;
            for sub in &flat.sub_calibrations {
                if sub.imagetyp == "DarkFlat" {
                    total_dark_flat_count += sub.frames.len() as i32;
                } else if sub.imagetyp == "Dark" {
                    total_dark_count += sub.frames.len() as i32;
                } else if sub.imagetyp == "Bias" {
                    total_bias_count += sub.frames.len() as i32;
                }
            }
            all_warnings.extend(flat.warnings.clone());
        }

        for dark in &dark_sets {
            total_dark_count += dark.frames.len() as i32;
            for sub in &dark.sub_calibrations {
                if sub.imagetyp == "Bias" {
                    total_bias_count += sub.frames.len() as i32;
                }
            }
            all_warnings.extend(dark.warnings.clone());
        }

        for bias in &bias_sets {
            total_bias_count += bias.frames.len() as i32;
            all_warnings.extend(bias.warnings.clone());
        }

        // Check completeness
        if flat_sets.is_empty() {
            flats_complete = false;
        }
        if dark_sets.is_empty() {
            darks_complete = false;
        }
        if bias_sets.is_empty() {
            bias_complete = false;
        }

        filters.push(FilterExportGroup {
            filter,
            light_frames: frames,
            flat_sets,
            dark_sets,
            bias_sets,
        });
    }

    // Sort filters alphabetically (None first, then by name)
    filters.sort_by(|a, b| match (&a.filter, &b.filter) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(a), Some(b)) => a.cmp(b),
    });

    // Calculate totals
    let total_light_frames: i32 = filters.iter().map(|f| f.light_frames.len() as i32).sum();
    let total_exposure_seconds: f64 = filters
        .iter()
        .flat_map(|f| &f.light_frames)
        .filter_map(|f| f.exptime)
        .sum();

    Ok(ExportData {
        frame_set_id,
        frame_set_name,
        object_name,
        groups,
        master_plan,
        filters,
        calibration_summary: CalibrationSummary {
            flat_count: total_flat_count,
            dark_count: total_dark_count,
            bias_count: total_bias_count,
            dark_flat_count: total_dark_flat_count,
            flats_complete,
            darks_complete,
            bias_complete,
            warnings: all_warnings,
        },
        total_light_frames,
        total_exposure_seconds,
    })
}

// ============================================================================
// Export Mode transform (spec §12.2)
// ============================================================================

/// Rewrite an already-collected [`ExportData`] for the chosen [`ExportMode`],
/// returning warnings to fold into the export summary.
///
/// - [`ExportMode::RawWithCalibrationSets`] (default): no change — the caller
///   gets today's behavior bit-for-bit (zero-regression path).
/// - [`ExportMode::LightsOnly`]: every calibration node is dropped; the raw
///   light paths are left exactly as collected.
/// - [`ExportMode::RawWithMasters`]: lights stay raw and only master files are
///   placed on the calibration side. Strict (spec 2026-08-28 D2): a linked set
///   that still has raw frames is an error, not an omission.
/// - [`ExportMode::CalibratedLights`]: each light's raw file is swapped for its
///   `light_calibrations.output_path` artifact (`c_*.fits`) and ALL calibration
///   nodes are dropped (WBPP runs with calibration disabled). Errors if any
///   in-scope light has no tracking row — the strict readiness gate (§12.2) must
///   run first at the API layer; this is the defensive backstop that guarantees
///   we never write a partial silent export.
pub fn apply_export_mode(
    conn: &Connection,
    data: &mut ExportData,
    mode: ExportMode,
) -> Result<Vec<String>> {
    tracing::debug!(frame_set_id = data.frame_set_id, ?mode, "applying export mode");
    match mode {
        ExportMode::RawWithCalibrationSets => Ok(Vec::new()),
        ExportMode::LightsOnly => {
            drop_calibration_nodes(data);
            Ok(Vec::new())
        }
        ExportMode::RawWithMasters => apply_raw_with_masters(conn, data),
        ExportMode::CalibratedLights => apply_calibrated_lights(conn, data),
    }
}

/// Clear every subgroup's calibration nodes (LightsOnly, and the first half of
/// CalibratedLights). Light frames are not touched.
fn drop_calibration_nodes(data: &mut ExportData) {
    for group in &mut data.groups {
        for subgroup in &mut group.subgroups {
            subgroup.flat = None;
            subgroup.dark = None;
            subgroup.bias = None;
        }
    }
}

/// Resolve the effective export mode for one export invocation.
///
/// An `explicit` per-invocation override always wins over the persisted
/// [`WbppExportConfig::export_mode`]; `None` falls back to the config (the
/// historical behavior). The mode used to travel *only* via the persisted
/// config, which the frontend loads asynchronously and can present as `null`
/// (slow/failed fetch) — in that window the mode-sync was skipped and the
/// backend silently exported the stale/default mode. Passing the selected mode
/// as an explicit arg closes that gap; both hosts (Tauri + web) resolve through
/// this one place so they stay in lockstep.
pub fn resolve_export_mode(explicit: Option<ExportMode>, config: &WbppExportConfig) -> ExportMode {
    explicit.unwrap_or(config.export_mode)
}

/// `is_master_library = 1` for `set_id`? A missing row (dangling link) counts as
/// not-a-master so its frames are dropped and reported, never silently kept.
fn is_master_set(conn: &Connection, set_id: i64) -> Result<bool> {
    let flag: Option<i64> = conn
        .query_row(
            "SELECT is_master_library FROM calibration_set WHERE id = ?1",
            [set_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(flag == Some(1))
}

/// Distinct ids of linked calibration sets that have frames but are not master
/// sets (`is_master_library = 0`), first-seen order. The `rawWithMasters`
/// readiness number (spec 2026-08-28 §5): an empty list = mode ready.
pub fn raw_sets_without_master(conn: &Connection, data: &ExportData) -> Result<Vec<i64>> {
    let mut seen: HashSet<i64> = HashSet::new();
    let mut out: Vec<i64> = Vec::new();
    for group in &data.groups {
        for subgroup in &group.subgroups {
            for node in [
                subgroup.flat.as_ref(),
                subgroup.dark.as_ref(),
                subgroup.bias.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                collect_raw_sets(conn, node, &mut seen, &mut out)?;
            }
        }
    }
    Ok(out)
}

/// Walk one calibration subtree, appending each raw-with-frames set id once.
fn collect_raw_sets(
    conn: &Connection,
    info: &CalibrationSetInfo,
    seen: &mut HashSet<i64>,
    out: &mut Vec<i64>,
) -> Result<()> {
    if !info.frames.is_empty() && !is_master_set(conn, info.set_id)? && seen.insert(info.set_id) {
        out.push(info.set_id);
    }
    for node in [
        info.dark_flat.as_deref(),
        info.dark.as_deref(),
        info.bias.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        collect_raw_sets(conn, node, seen, out)?;
    }
    Ok(())
}

/// Count-only walk for `ExportReadiness.file_counts` (spec 2026-08-28 §5): what
/// each mode would place, never bailing. `raw_with_masters` counts a raw set as
/// zero files (strict mode would refuse it) — the count is informational, the
/// gate is `check_mode_ready`.
pub fn export_file_counts(conn: &Connection, data: &ExportData) -> Result<ExportFileCounts> {
    use crate::export::file_organizer::compute_wbpp_placements;
    let lights: i64 = data
        .groups
        .iter()
        .flat_map(|g| g.subgroups.iter())
        .map(|sg| sg.frames.len() as i64)
        .sum();
    let raw_with_calibration_sets = compute_wbpp_placements(data).len() as i64;
    let mut masters_only = data.clone();
    for group in &mut masters_only.groups {
        for subgroup in &mut group.subgroups {
            for node in [
                subgroup.flat.as_mut(),
                subgroup.dark.as_mut(),
                subgroup.bias.as_mut(),
            ]
            .into_iter()
            .flatten()
            {
                clear_raw_frames_recursive(conn, node)?;
            }
        }
    }
    let raw_with_masters = compute_wbpp_placements(&masters_only).len() as i64;
    Ok(ExportFileCounts {
        lights_only: lights,
        raw_with_calibration_sets,
        raw_with_masters,
        calibrated_lights: lights,
    })
}

/// Count helper: empty the frames of every non-master set in one subtree.
fn clear_raw_frames_recursive(conn: &Connection, info: &mut CalibrationSetInfo) -> Result<()> {
    if !is_master_set(conn, info.set_id)? {
        info.frames.clear();
        info.frame_count = 0;
    }
    for node in [
        info.dark_flat.as_mut(),
        info.dark.as_mut(),
        info.bias.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
        clear_raw_frames_recursive(conn, node)?;
    }
    Ok(())
}

/// Strict (spec 2026-08-28 D2): every linked set with frames must be a master.
/// The API-layer gate runs first; this is the backstop that guarantees a
/// partial export/send can never be written.
fn apply_raw_with_masters(conn: &Connection, data: &mut ExportData) -> Result<Vec<String>> {
    let raw = raw_sets_without_master(conn, data)?;
    if !raw.is_empty() {
        anyhow::bail!(
            "{} calibration set(s) have no master — build masters first (sets {:?})",
            raw.len(),
            raw
        );
    }
    Ok(Vec::new())
}

fn apply_calibrated_lights(conn: &Connection, data: &mut ExportData) -> Result<Vec<String>> {
    // No calibration frames are exported — WBPP runs with calibration disabled,
    // so the BIAS/DARKS/FLAT nesting is dropped entirely and the lights land
    // directly under the camera folder.
    drop_calibration_nodes(data);
    let mut missing: Vec<i64> = Vec::new();
    let mut total = 0usize;
    for group in &mut data.groups {
        for subgroup in &mut group.subgroups {
            for frame in &mut subgroup.frames {
                total += 1;
                match get_light_calibration_for_frame(conn, frame.frame_id)? {
                    Some(row) => {
                        let filename = Path::new(&row.output_path)
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| format!("c_{}", frame.filename));
                        frame.file_path = row.output_path;
                        frame.filename = filename;
                    }
                    None => missing.push(frame.frame_id),
                }
            }
        }
    }
    if !missing.is_empty() {
        anyhow::bail!(
            "{} of {} lights lack a fresh calibrated output — run Calibrate Lights first",
            missing.len(),
            total
        );
    }
    tracing::debug!(
        frame_set_id = data.frame_set_id,
        lights = total,
        "calibrated-lights mode: substituted artifact paths"
    );
    Ok(Vec::new())
}

/// Get frame set name and object name
fn get_frame_set_info(conn: &Connection, frame_set_id: i64) -> Result<(String, Option<String>)> {
    let result = conn.query_row(
        "SELECT name FROM frames_set WHERE id = ?1",
        [frame_set_id],
        |row| {
            let name: Option<String> = row.get(0)?;
            Ok(name.unwrap_or_else(|| format!("Frame Set {}", frame_set_id)))
        },
    )?;

    // Get object name from first light frame in the set
    let object_name: Option<String> = conn
        .query_row(
            "SELECT f.object
             FROM frames f
             JOIN session_members sm ON f.id = sm.frame_id
             JOIN sessions s ON sm.session_id = s.id
             JOIN imaging_nights i ON s.imaging_night_id = i.id
             WHERE i.frames_set_id = ?1
               AND f.imagetyp = 'Light'
               AND f.object IS NOT NULL
             LIMIT 1",
            [frame_set_id],
            |row| row.get(0),
        )
        .ok();

    Ok((result, object_name))
}

/// Get all light frames from a frame set via the hierarchy
fn get_light_frames_for_frame_set(conn: &Connection, frame_set_id: i64) -> Result<Vec<ExportFrame>> {
    // First get all frame IDs using a simpler query (proven pattern from calibration_links.rs)
    let mut id_stmt = conn.prepare(
        "SELECT DISTINCT sm.frame_id
         FROM session_members sm
         JOIN sessions s ON sm.session_id = s.id
         JOIN imaging_nights n ON s.imaging_night_id = n.id
         JOIN frames f ON sm.frame_id = f.id
         WHERE n.frames_set_id = ?1 AND f.imagetyp = 'Light'"
    )?;

    let frame_ids: Vec<i64> = id_stmt
        .query_map([frame_set_id], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    tracing::debug!(frame_set_id, count = frame_ids.len(), "frame IDs found via session_members");

    // Now get full frame info for each ID
    let mut frames = Vec::new();
    for frame_id in &frame_ids {
        if let Ok(frame) = get_export_frame_by_id(conn, *frame_id) {
            frames.push(frame);
        }
    }

    tracing::debug!(frame_set_id, count = frames.len(), "full frame records loaded");
    Ok(frames)
}

/// Get a single frame with file info by ID
fn get_export_frame_by_id(conn: &Connection, frame_id: i64) -> Result<ExportFrame> {
    conn.query_row(
        "SELECT f.id, f.file_id, fi.path, fi.filename,
                f.exptime, f.filter, f.ccd_temp, f.gain, f.offset,
                f.binning, f.date_obs, f.focallen, f.xpixsz, f.bayerpat, f.instrume
         FROM frames f
         JOIN files fi ON f.file_id = fi.id
         WHERE f.id = ?1",
        [frame_id],
        |row| {
            Ok(ExportFrame {
                frame_id: row.get(0)?,
                file_id: row.get(1)?,
                file_path: row.get(2)?,
                filename: row.get(3)?,
                exptime: row.get(4)?,
                filter: row.get(5)?,
                ccd_temp: row.get(6)?,
                gain: row.get(7)?,
                offset: row.get(8)?,
                binning: row.get(9)?,
                date_obs: row.get(10)?,
                focallen: row.get(11)?,
                xpixsz: row.get(12)?,
                bayerpat: row.get(13)?,
                instrume: row.get(14)?,
            })
        },
    ).context(format!("Failed to get frame by ID: {}", frame_id))
}

/// Collect calibrations for a single frame
/// Returns (flats, darks, bias)
fn collect_calibrations_for_frame(
    conn: &Connection,
    frame_id: i64,
) -> Result<(
    Vec<ExportCalibrationSet>,
    Vec<ExportCalibrationSet>,
    Vec<ExportCalibrationSet>,
)> {
    tracing::debug!(frame_id, "collecting calibrations for frame");
    let mut flat_sets = Vec::new();
    let mut dark_sets = Vec::new();
    let mut bias_sets = Vec::new();

    // Get calibration links for this frame
    let mut stmt = conn.prepare(
        "SELECT calibration_set_id, calibration_type, match_score,
                date_warning, temp_warning
         FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'frame'",
    )?;

    let links: Vec<(i64, String, Option<f64>, bool, bool)> = stmt
        .query_map([frame_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i32>(3)? != 0,
                row.get::<_, i32>(4)? != 0,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    tracing::debug!(frame_id, count = links.len(), "calibration links found");
    for (set_id, cal_type, match_score, date_warning, temp_warning) in links {
        tracing::debug!(frame_id, set_id, calibration_type = %cal_type, score = ?match_score, "calibration link");
        let cal_set = build_calibration_set(conn, set_id, match_score, date_warning, temp_warning)?;

        match cal_type.as_str() {
            "Flat" => flat_sets.push(cal_set),
            "Dark" => dark_sets.push(cal_set),
            "Bias" => bias_sets.push(cal_set),
            _ => {}
        }
    }

    tracing::debug!(
        frame_id,
        flats = flat_sets.len(),
        darks = dark_sets.len(),
        bias = bias_sets.len(),
        "calibrations collected for frame"
    );
    Ok((flat_sets, dark_sets, bias_sets))
}

/// Build an ExportCalibrationSet with its frames and sub-calibrations
fn build_calibration_set(
    conn: &Connection,
    set_id: i64,
    match_score: Option<f64>,
    date_warning: bool,
    temp_warning: bool,
) -> Result<ExportCalibrationSet> {
    // Get calibration set info
    let imagetyp: String = conn
        .query_row(
            "SELECT imagetyp FROM calibration_set WHERE id = ?1",
            [set_id],
            |row| row.get(0),
        )
        .context("Failed to get calibration set")?;

    // Get frames in the calibration set
    let frames = get_calibration_set_frames(conn, set_id)?;

    // Build warnings list
    let mut warnings = Vec::new();
    if date_warning {
        warnings.push("Date warning: calibration may be too old".to_string());
    }
    if temp_warning {
        warnings.push("Temperature warning: temperature mismatch detected".to_string());
    }

    // Get sub-calibrations (recursively)
    let sub_calibrations = get_sub_calibrations(conn, set_id)?;

    Ok(ExportCalibrationSet {
        set_id,
        imagetyp,
        frames,
        sub_calibrations,
        match_score,
        warnings,
    })
}

/// Get frames belonging to a calibration set
fn get_calibration_set_frames(conn: &Connection, set_id: i64) -> Result<Vec<ExportFrame>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.file_id, fi.path, fi.filename,
                f.exptime, f.filter, f.ccd_temp, f.gain, f.offset,
                f.binning, f.date_obs, f.focallen, f.xpixsz, f.bayerpat, f.instrume
         FROM frames f
         JOIN files fi ON f.file_id = fi.id
         JOIN calibration_set_frames csf ON f.id = csf.frame_id
         WHERE csf.set_id = ?1
         ORDER BY f.date_obs ASC",
    )?;

    let frames = stmt
        .query_map([set_id], |row| {
            Ok(ExportFrame {
                frame_id: row.get(0)?,
                file_id: row.get(1)?,
                file_path: row.get(2)?,
                filename: row.get(3)?,
                exptime: row.get(4)?,
                filter: row.get(5)?,
                ccd_temp: row.get(6)?,
                gain: row.get(7)?,
                offset: row.get(8)?,
                binning: row.get(9)?,
                date_obs: row.get(10)?,
                focallen: row.get(11)?,
                xpixsz: row.get(12)?,
                bayerpat: row.get(13)?,
                instrume: row.get(14)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(frames)
}

/// Get sub-calibrations for a calibration set (e.g., Dark for Flat, Bias for Dark)
fn get_sub_calibrations(conn: &Connection, set_id: i64) -> Result<Vec<ExportCalibrationSet>> {
    let mut sub_calibrations = Vec::new();

    // Get calibration links where this set is the source
    let mut stmt = conn.prepare(
        "SELECT calibration_set_id, calibration_type, match_score,
                date_warning, temp_warning
         FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'calibration_set'",
    )?;

    let links: Vec<(i64, String, Option<f64>, bool, bool)> = stmt
        .query_map([set_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i32>(3)? != 0,
                row.get::<_, i32>(4)? != 0,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    for (sub_set_id, _cal_type, match_score, date_warning, temp_warning) in links {
        // Recursively build sub-calibration (without further recursion to avoid infinite loops)
        let sub_set = build_calibration_set_shallow(conn, sub_set_id, match_score, date_warning, temp_warning)?;
        sub_calibrations.push(sub_set);
    }

    Ok(sub_calibrations)
}

/// Build a calibration set without recursive sub-calibrations (to avoid deep recursion)
fn build_calibration_set_shallow(
    conn: &Connection,
    set_id: i64,
    match_score: Option<f64>,
    date_warning: bool,
    temp_warning: bool,
) -> Result<ExportCalibrationSet> {
    // Get calibration set info
    let imagetyp: String = conn
        .query_row(
            "SELECT imagetyp FROM calibration_set WHERE id = ?1",
            [set_id],
            |row| row.get(0),
        )
        .context("Failed to get calibration set")?;

    // Get frames
    let frames = get_calibration_set_frames(conn, set_id)?;

    // Build warnings
    let mut warnings = Vec::new();
    if date_warning {
        warnings.push("Date warning: calibration may be too old".to_string());
    }
    if temp_warning {
        warnings.push("Temperature warning: temperature mismatch detected".to_string());
    }

    Ok(ExportCalibrationSet {
        set_id,
        imagetyp,
        frames,
        sub_calibrations: Vec::new(), // No recursive sub-calibrations
        match_score,
        warnings,
    })
}

// ============================================================================
// Phase 3: New Export Group Building Logic
// ============================================================================

/// Calibration links for a single frame
#[derive(Debug, Clone)]
struct FrameCalibrationLinks {
    #[allow(dead_code)]
    frame_id: i64,
    flat_id: Option<i64>,
    dark_id: Option<i64>,
    bias_id: Option<i64>,
}

impl FrameCalibrationLinks {
    /// Generate a subgroup key from calibration set IDs
    fn subgroup_key(&self) -> String {
        format!(
            "f{}_d{}_b{}",
            self.flat_id.map(|id| id.to_string()).unwrap_or_else(|| "none".to_string()),
            self.dark_id.map(|id| id.to_string()).unwrap_or_else(|| "none".to_string()),
            self.bias_id.map(|id| id.to_string()).unwrap_or_else(|| "none".to_string()),
        )
    }
}

/// Get calibration links for a single frame
fn get_frame_calibration_links(conn: &Connection, frame_id: i64) -> Result<FrameCalibrationLinks> {
    let mut stmt = conn.prepare(
        "SELECT calibration_set_id, calibration_type
         FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'frame'",
    )?;

    let links: Vec<(i64, String)> = stmt
        .query_map([frame_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let mut flat_id = None;
    let mut dark_id = None;
    let mut bias_id = None;

    for (set_id, cal_type) in links {
        match cal_type.as_str() {
            "Flat" => flat_id = Some(set_id),
            "Dark" => dark_id = Some(set_id),
            "Bias" => bias_id = Some(set_id),
            _ => {}
        }
    }

    Ok(FrameCalibrationLinks {
        frame_id,
        flat_id,
        dark_id,
        bias_id,
    })
}

/// Build CalibrationSetInfo with recursive sub-calibrations
fn build_calibration_set_info(
    conn: &Connection,
    set_id: i64,
    match_score: Option<f64>,
    date_warning: bool,
    temp_warning: bool,
) -> Result<CalibrationSetInfo> {
    // Get calibration set info
    let imagetyp: String = conn
        .query_row(
            "SELECT imagetyp FROM calibration_set WHERE id = ?1",
            [set_id],
            |row| row.get(0),
        )
        .context("Failed to get calibration set")?;

    // Get frames in the calibration set
    let frames = get_calibration_set_frames(conn, set_id)?;
    let frame_count = frames.len() as i32;

    // Build warnings list
    let mut warnings = Vec::new();
    if date_warning {
        warnings.push("Date warning: calibration may be too old".to_string());
    }
    if temp_warning {
        warnings.push("Temperature warning: temperature mismatch detected".to_string());
    }

    // Get sub-calibrations for this set
    let mut dark_flat: Option<Box<CalibrationSetInfo>> = None;
    let mut dark: Option<Box<CalibrationSetInfo>> = None;
    let mut bias: Option<Box<CalibrationSetInfo>> = None;

    let mut stmt = conn.prepare(
        "SELECT calibration_set_id, calibration_type, match_score, date_warning, temp_warning
         FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'calibration_set'",
    )?;

    let sub_links: Vec<(i64, String, Option<f64>, bool, bool)> = stmt
        .query_map([set_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i32>(3)? != 0,
                row.get::<_, i32>(4)? != 0,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    for (sub_set_id, cal_type, sub_score, sub_date_warn, sub_temp_warn) in sub_links {
        // Build sub-calibration (one level deep to avoid infinite recursion)
        let sub_info = build_calibration_set_info_shallow(conn, sub_set_id, sub_score, sub_date_warn, sub_temp_warn)?;

        match cal_type.as_str() {
            "DarkFlat" => dark_flat = Some(Box::new(sub_info)),
            "Dark" => dark = Some(Box::new(sub_info)),
            "Bias" => bias = Some(Box::new(sub_info)),
            _ => {}
        }
    }

    Ok(CalibrationSetInfo {
        set_id,
        imagetyp,
        frames,
        frame_count,
        dark_flat,
        dark,
        bias,
        match_score,
        warnings,
    })
}

/// Build CalibrationSetInfo without deep recursion (for sub-calibrations)
fn build_calibration_set_info_shallow(
    conn: &Connection,
    set_id: i64,
    match_score: Option<f64>,
    date_warning: bool,
    temp_warning: bool,
) -> Result<CalibrationSetInfo> {
    let imagetyp: String = conn
        .query_row(
            "SELECT imagetyp FROM calibration_set WHERE id = ?1",
            [set_id],
            |row| row.get(0),
        )
        .context("Failed to get calibration set")?;

    let frames = get_calibration_set_frames(conn, set_id)?;
    let frame_count = frames.len() as i32;

    let mut warnings = Vec::new();
    if date_warning {
        warnings.push("Date warning: calibration may be too old".to_string());
    }
    if temp_warning {
        warnings.push("Temperature warning: temperature mismatch detected".to_string());
    }

    // Get sub-calibrations (bias for darks)
    let mut bias: Option<Box<CalibrationSetInfo>> = None;

    let mut stmt = conn.prepare(
        "SELECT calibration_set_id, calibration_type, match_score, date_warning, temp_warning
         FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'calibration_set' AND calibration_type = 'Bias'",
    )?;

    if let Ok((bias_id, _, bias_score, bias_date, bias_temp)) = stmt.query_row([set_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<f64>>(2)?,
            row.get::<_, i32>(3)? != 0,
            row.get::<_, i32>(4)? != 0,
        ))
    }) {
        // Build bias (terminal node, no further sub-calibrations)
        let bias_info = build_calibration_set_info_terminal(conn, bias_id, bias_score, bias_date, bias_temp)?;
        bias = Some(Box::new(bias_info));
    }

    Ok(CalibrationSetInfo {
        set_id,
        imagetyp,
        frames,
        frame_count,
        dark_flat: None,
        dark: None,
        bias,
        match_score,
        warnings,
    })
}

/// Build CalibrationSetInfo as terminal node (no sub-calibrations)
fn build_calibration_set_info_terminal(
    conn: &Connection,
    set_id: i64,
    match_score: Option<f64>,
    date_warning: bool,
    temp_warning: bool,
) -> Result<CalibrationSetInfo> {
    let imagetyp: String = conn
        .query_row(
            "SELECT imagetyp FROM calibration_set WHERE id = ?1",
            [set_id],
            |row| row.get(0),
        )
        .context("Failed to get calibration set")?;

    let frames = get_calibration_set_frames(conn, set_id)?;
    let frame_count = frames.len() as i32;

    let mut warnings = Vec::new();
    if date_warning {
        warnings.push("Date warning: calibration may be too old".to_string());
    }
    if temp_warning {
        warnings.push("Temperature warning: temperature mismatch detected".to_string());
    }

    Ok(CalibrationSetInfo {
        set_id,
        imagetyp,
        frames,
        frame_count,
        dark_flat: None,
        dark: None,
        bias: None,
        match_score,
        warnings,
    })
}

/// Get CalibrationSetInfo for a frame's calibration link
fn get_calibration_set_info_for_frame(
    conn: &Connection,
    frame_id: i64,
    cal_type: &str,
) -> Result<Option<CalibrationSetInfo>> {
    let result: Option<(i64, Option<f64>, bool, bool)> = conn
        .query_row(
            "SELECT calibration_set_id, match_score, date_warning, temp_warning
             FROM calibration_set_to_frames
             WHERE source_id = ?1 AND source_type = 'frame' AND calibration_type = ?2",
            rusqlite::params![frame_id, cal_type],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get::<_, i32>(2)? != 0,
                    row.get::<_, i32>(3)? != 0,
                ))
            },
        )
        .ok();

    match result {
        Some((set_id, match_score, date_warning, temp_warning)) => {
            let info = build_calibration_set_info(conn, set_id, match_score, date_warning, temp_warning)?;
            Ok(Some(info))
        }
        None => Ok(None),
    }
}

/// Build export groups from light frames
/// Groups by (filter, camera_type) and creates subgroups by calibration links
fn build_export_groups(conn: &Connection, light_frames: &[ExportFrame]) -> Result<Vec<ExportGroup>> {
    tracing::debug!(count = light_frames.len(), "building export groups from light frames");

    // Group key: (filter, camera_type)
    type GroupKey = (Option<String>, CameraType);
    let mut groups_map: HashMap<GroupKey, Vec<&ExportFrame>> = HashMap::new();

    // Group frames by filter and camera type
    for frame in light_frames {
        let camera_type = CameraType::from_bayerpat(frame.bayerpat.as_deref());
        let key = (frame.filter.clone(), camera_type);
        groups_map.entry(key).or_default().push(frame);
    }

    tracing::debug!(count = groups_map.len(), "distinct (filter, camera_type) groups found");

    let mut export_groups = Vec::new();

    for ((filter, camera_type), frames) in groups_map {
        let group_key = ExportGroup::make_group_key(filter.as_deref(), &camera_type);
        let display_name = ExportGroup::make_display_name(filter.as_deref(), &camera_type);

        tracing::debug!(group = %display_name, count = frames.len(), "building export group");

        // Build subgroups within this group
        let subgroups = build_calibration_subgroups(conn, &frames)?;

        // Calculate totals
        let total_frames = frames.len() as i32;
        let total_exposure: f64 = frames.iter().filter_map(|f| f.exptime).sum();

        // Collect warnings
        let mut warnings = Vec::new();
        for subgroup in &subgroups {
            warnings.extend(subgroup.warnings.clone());
        }

        export_groups.push(ExportGroup {
            group_key,
            filter,
            camera_type,
            display_name,
            subgroups,
            total_frames,
            total_exposure,
            warnings,
        });
    }

    // Sort groups by display name for consistent ordering
    export_groups.sort_by(|a, b| a.display_name.cmp(&b.display_name));

    Ok(export_groups)
}

/// Build calibration subgroups within an export group
/// Groups frames by their linked calibration set IDs
fn build_calibration_subgroups(
    conn: &Connection,
    frames: &[&ExportFrame],
) -> Result<Vec<CalibrationSubgroup>> {
    // Get calibration links for each frame
    let mut frame_links: HashMap<i64, FrameCalibrationLinks> = HashMap::new();
    for frame in frames {
        let links = get_frame_calibration_links(conn, frame.frame_id)?;
        frame_links.insert(frame.frame_id, links);
    }

    // Group frames by subgroup key (combination of calibration set IDs)
    let mut subgroup_map: HashMap<String, Vec<&ExportFrame>> = HashMap::new();
    for frame in frames {
        if let Some(links) = frame_links.get(&frame.frame_id) {
            let key = links.subgroup_key();
            subgroup_map.entry(key).or_default().push(*frame);
        }
    }

    let subgroup_count = subgroup_map.len();
    tracing::debug!(count = subgroup_count, "calibration subgroups found");

    let mut subgroups = Vec::new();
    let mut subgroup_index = 1;

    for (subgroup_key, subgroup_frames) in subgroup_map {
        // Get the calibration links from the first frame (all frames in subgroup share the same links)
        let first_frame = subgroup_frames.first().unwrap();
        let links = frame_links.get(&first_frame.frame_id).unwrap();

        // Build CalibrationSetInfo for each calibration type
        let flat = if links.flat_id.is_some() {
            get_calibration_set_info_for_frame(conn, first_frame.frame_id, "Flat")?
        } else {
            None
        };

        let dark = if links.dark_id.is_some() {
            get_calibration_set_info_for_frame(conn, first_frame.frame_id, "Dark")?
        } else {
            None
        };

        let bias = if links.bias_id.is_some() {
            get_calibration_set_info_for_frame(conn, first_frame.frame_id, "Bias")?
        } else {
            None
        };

        // Generate display name
        let display_name = if subgroup_count == 1 {
            "Default".to_string()
        } else {
            format!("Subgroup {}", subgroup_index)
        };

        // Collect warnings
        let mut warnings = Vec::new();
        if let Some(ref f) = flat {
            warnings.extend(f.warnings.clone());
        }
        if let Some(ref d) = dark {
            warnings.extend(d.warnings.clone());
        }
        if let Some(ref b) = bias {
            warnings.extend(b.warnings.clone());
        }

        subgroups.push(CalibrationSubgroup {
            subgroup_key,
            display_name,
            frames: subgroup_frames.iter().map(|f| (*f).clone()).collect(),
            flat,
            dark,
            bias,
            warnings,
        });

        subgroup_index += 1;
    }

    // Sort subgroups by display name
    subgroups.sort_by(|a, b| a.display_name.cmp(&b.display_name));

    Ok(subgroups)
}

/// Build the master creation plan from export groups
/// Collects all unique calibration sets and topologically sorts by dependencies
fn build_master_creation_plan(
    conn: &Connection,
    groups: &[ExportGroup],
) -> Result<MasterCreationPlan> {
    tracing::debug!("building master creation plan");

    // Collect all unique calibration set IDs with their types
    let mut all_sets: HashMap<i64, String> = HashMap::new(); // set_id -> imagetyp
    let mut dependencies: HashMap<i64, Vec<i64>> = HashMap::new(); // set_id -> depends on set_ids

    // Traverse all groups and subgroups to collect calibration sets
    for group in groups {
        for subgroup in &group.subgroups {
            // Collect from flat
            if let Some(ref flat) = subgroup.flat {
                collect_calibration_sets_recursive(flat, &mut all_sets, &mut dependencies);
            }
            // Collect from dark
            if let Some(ref dark) = subgroup.dark {
                collect_calibration_sets_recursive(dark, &mut all_sets, &mut dependencies);
            }
            // Collect from bias
            if let Some(ref bias) = subgroup.bias {
                collect_calibration_sets_recursive(bias, &mut all_sets, &mut dependencies);
            }
        }
    }

    tracing::debug!(count = all_sets.len(), "unique calibration sets found");

    // Topological sort to determine creation order
    let sorted_ids = topological_sort(&all_sets, &dependencies);

    tracing::debug!(count = sorted_ids.len(), "master creation plan topologically sorted");

    // Build MasterInfo for each set
    let mut masters = Vec::new();
    let mut master_paths: HashMap<i64, String> = HashMap::new();

    for set_id in sorted_ids {
        if let Some(imagetyp) = all_sets.get(&set_id) {
            // Get frames for this set
            let frames = get_calibration_set_frames(conn, set_id)?;

            // Calculate source exposure time (average of source frames)
            let source_exptime = if !frames.is_empty() {
                let sum: f64 = frames.iter().filter_map(|f| f.exptime).sum();
                let count = frames.iter().filter(|f| f.exptime.is_some()).count();
                if count > 0 {
                    Some(sum / count as f64)
                } else {
                    None
                }
            } else {
                None
            };

            // Determine dependencies
            let deps = dependencies.get(&set_id).cloned().unwrap_or_default();

            // Determine which calibrations to apply (with exposure matching for flats)
            let (apply_bias, apply_dark, apply_darkflat) =
                get_calibration_applications(conn, set_id, imagetyp, source_exptime)?;

            // Generate output filename
            let output_name = format!("master_{}_{}.fit", imagetyp.to_lowercase(), set_id);
            master_paths.insert(set_id, output_name.clone());

            masters.push(MasterInfo {
                set_id,
                master_type: imagetyp.clone(),
                output_name,
                source_frames: frames,
                depends_on: deps,
                apply_bias,
                apply_dark,
                apply_darkflat,
                source_exptime,
            });
        }
    }

    Ok(MasterCreationPlan {
        masters,
        master_paths,
    })
}

/// Recursively collect calibration sets from a CalibrationSetInfo hierarchy
fn collect_calibration_sets_recursive(
    info: &CalibrationSetInfo,
    all_sets: &mut HashMap<i64, String>,
    dependencies: &mut HashMap<i64, Vec<i64>>,
) {
    // Add this set
    all_sets.insert(info.set_id, info.imagetyp.clone());

    // Track dependencies
    let mut deps = Vec::new();

    // Recurse into sub-calibrations
    if let Some(ref dark_flat) = info.dark_flat {
        deps.push(dark_flat.set_id);
        collect_calibration_sets_recursive(dark_flat, all_sets, dependencies);
    }
    if let Some(ref dark) = info.dark {
        deps.push(dark.set_id);
        collect_calibration_sets_recursive(dark, all_sets, dependencies);
    }
    if let Some(ref bias) = info.bias {
        deps.push(bias.set_id);
        collect_calibration_sets_recursive(bias, all_sets, dependencies);
    }

    if !deps.is_empty() {
        dependencies.insert(info.set_id, deps);
    }
}

/// Topological sort of calibration sets by dependencies
/// Returns set IDs in order they should be created (dependencies first)
fn topological_sort(
    all_sets: &HashMap<i64, String>,
    dependencies: &HashMap<i64, Vec<i64>>,
) -> Vec<i64> {
    let mut result = Vec::new();
    let mut visited: HashSet<i64> = HashSet::new();
    let mut temp_mark: HashSet<i64> = HashSet::new();

    fn visit(
        node: i64,
        dependencies: &HashMap<i64, Vec<i64>>,
        visited: &mut HashSet<i64>,
        temp_mark: &mut HashSet<i64>,
        result: &mut Vec<i64>,
    ) {
        if visited.contains(&node) {
            return;
        }
        if temp_mark.contains(&node) {
            // Cycle detected, skip (shouldn't happen with proper calibration hierarchy)
            return;
        }

        temp_mark.insert(node);

        // Visit dependencies first
        if let Some(deps) = dependencies.get(&node) {
            for dep in deps {
                visit(*dep, dependencies, visited, temp_mark, result);
            }
        }

        temp_mark.remove(&node);
        visited.insert(node);
        result.push(node);
    }

    for &set_id in all_sets.keys() {
        visit(set_id, dependencies, &mut visited, &mut temp_mark, &mut result);
    }

    result
}

/// Exposure time tolerance for flat→dark calibration matching (30% = 0.30)
const FLAT_DARK_EXPOSURE_TOLERANCE: f64 = 0.30;

/// Check if two exposure times are within tolerance
/// Returns true if they are similar enough for calibration purposes
fn is_exposure_match(source_exptime: f64, cal_exptime: f64, tolerance_pct: f64) -> bool {
    if source_exptime <= 0.0 || cal_exptime <= 0.0 {
        return false;
    }
    let max_exp = source_exptime.max(cal_exptime);
    let diff_ratio = (source_exptime - cal_exptime).abs() / max_exp;
    diff_ratio <= tolerance_pct
}

/// Get the average exposure time for a calibration set
fn get_calibration_set_exptime(conn: &Connection, set_id: i64) -> Result<Option<f64>> {
    let result: Option<f64> = conn
        .query_row(
            "SELECT AVG(f.exptime)
             FROM frames f
             JOIN calibration_set_frames csf ON f.id = csf.frame_id
             WHERE csf.set_id = ?1 AND f.exptime IS NOT NULL",
            [set_id],
            |row| row.get(0),
        )
        .ok()
        .flatten();
    Ok(result)
}

/// Get which calibrations should be applied when creating a master
/// Returns (apply_bias, apply_dark, apply_darkflat) set IDs
/// Note: Dark and DarkFlat are tracked separately because:
/// - Dark is used for light frame calibration (long exposure matching lights)
/// - DarkFlat is used for flat frame calibration (short exposure matching flats)
///
/// For Flat calibration:
/// - Priority 1: DarkFlat (same exposure as flat)
/// - Priority 2: Dark with matching exposure time (±30%)
/// - Priority 3: Bias (if no matching dark)
/// - Otherwise: skip dark calibration
fn get_calibration_applications(
    conn: &Connection,
    set_id: i64,
    imagetyp: &str,
    source_exptime: Option<f64>,
) -> Result<(Option<i64>, Option<i64>, Option<i64>)> {
    let mut apply_bias = None;
    let mut apply_dark = None;
    let mut apply_darkflat = None;

    let mut stmt = conn.prepare(
        "SELECT calibration_set_id, calibration_type
         FROM calibration_set_to_frames
         WHERE source_id = ?1 AND source_type = 'calibration_set'",
    )?;

    let links: Vec<(i64, String)> = stmt
        .query_map([set_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    for (cal_id, cal_type) in links {
        match cal_type.as_str() {
            "Bias" => apply_bias = Some(cal_id),
            "Dark" => {
                // For Flat calibration, only use dark if exposure time matches
                if imagetyp == "Flat" {
                    if let Some(flat_exptime) = source_exptime {
                        if let Ok(Some(dark_exptime)) = get_calibration_set_exptime(conn, cal_id) {
                            if is_exposure_match(flat_exptime, dark_exptime, FLAT_DARK_EXPOSURE_TOLERANCE) {
                                apply_dark = Some(cal_id);
                                tracing::debug!(
                                    set_id = cal_id,
                                    flat_exptime,
                                    dark_exptime,
                                    "flat-dark exposure match"
                                );
                            } else {
                                tracing::warn!(
                                    set_id = cal_id,
                                    flat_exptime,
                                    dark_exptime,
                                    "flat-dark exposure mismatch, falling back to bias"
                                );
                            }
                        }
                    }
                } else {
                    // For non-Flat calibration (e.g., Dark→Bias), use dark as-is
                    apply_dark = Some(cal_id);
                }
            }
            "DarkFlat" => apply_darkflat = Some(cal_id),
            _ => {}
        }
    }

    Ok((apply_bias, apply_dark, apply_darkflat))
}

// ============================================================================
// Export Summary Builder (Enhanced UI)
// ============================================================================

/// Collect enhanced export summary for the new UI
pub fn collect_export_summary(conn: &Connection, frame_set_id: i64, config: &WbppExportConfig) -> Result<ExportSummary> {
    tracing::debug!(frame_set_id, "building export summary");

    // Get basic export data first
    let export_data = collect_export_data(conn, frame_set_id)?;

    // Collect equipment info from all frames
    let (cameras, telescopes, date_range) = collect_equipment_info(conn, frame_set_id)?;
    tracing::debug!(
        frame_set_id,
        cameras = cameras.len(),
        telescopes = telescopes.len(),
        "equipment info collected"
    );

    // Build filter group summaries
    let filter_groups = build_filter_group_summaries(conn, &export_data)?;
    tracing::debug!(frame_set_id, count = filter_groups.len(), "filter groups built");

    // Build folder preview
    let folder_preview = build_folder_preview(&export_data, config)?;

    // Build detailed warnings
    let warnings = build_detailed_warnings(&export_data, &filter_groups)?;
    tracing::debug!(frame_set_id, count = warnings.len(), "export warnings generated");

    // Calculate totals
    let total_files = calculate_total_files(&export_data);
    let estimated_size_bytes = estimate_total_size(conn, &export_data)?;

    Ok(ExportSummary {
        frame_set_id,
        frame_set_name: export_data.frame_set_name.clone(),
        object_name: export_data.object_name.clone(),
        cameras,
        telescopes,
        date_range,
        filter_groups,
        folder_preview,
        warnings,
        total_files,
        estimated_size_bytes,
    })
}

/// Collect unique equipment (cameras, telescopes) and date range from a frame set
fn collect_equipment_info(
    conn: &Connection,
    frame_set_id: i64,
) -> Result<(Vec<String>, Vec<String>, Option<(String, String)>)> {
    // Get unique cameras
    let mut cameras_stmt = conn.prepare(
        "SELECT DISTINCT f.instrume
         FROM frames f
         JOIN session_members sm ON f.id = sm.frame_id
         JOIN sessions s ON sm.session_id = s.id
         JOIN imaging_nights n ON s.imaging_night_id = n.id
         WHERE n.frames_set_id = ?1 AND f.instrume IS NOT NULL
         ORDER BY f.instrume",
    )?;
    let cameras: Vec<String> = cameras_stmt
        .query_map([frame_set_id], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    // Get unique telescopes
    let mut telescopes_stmt = conn.prepare(
        "SELECT DISTINCT f.telescop
         FROM frames f
         JOIN session_members sm ON f.id = sm.frame_id
         JOIN sessions s ON sm.session_id = s.id
         JOIN imaging_nights n ON s.imaging_night_id = n.id
         WHERE n.frames_set_id = ?1 AND f.telescop IS NOT NULL
         ORDER BY f.telescop",
    )?;
    let telescopes: Vec<String> = telescopes_stmt
        .query_map([frame_set_id], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    // Get date range
    let date_range: Option<(String, String)> = conn
        .query_row(
            "SELECT MIN(f.date_obs), MAX(f.date_obs)
             FROM frames f
             JOIN session_members sm ON f.id = sm.frame_id
             JOIN sessions s ON sm.session_id = s.id
             JOIN imaging_nights n ON s.imaging_night_id = n.id
             WHERE n.frames_set_id = ?1 AND f.date_obs IS NOT NULL",
            [frame_set_id],
            |row| {
                let min: Option<String> = row.get(0)?;
                let max: Option<String> = row.get(1)?;
                Ok(min.and_then(|m| max.map(|x| (m, x))))
            },
        )
        .ok()
        .flatten();

    Ok((cameras, telescopes, date_range))
}

/// Build filter group summaries from export data
fn build_filter_group_summaries(
    conn: &Connection,
    export_data: &ExportData,
) -> Result<Vec<FilterGroupSummary>> {
    let mut summaries = Vec::new();

    for group in &export_data.groups {
        // Get all frames from all subgroups
        let all_frames: Vec<&ExportFrame> = group.subgroups.iter().flat_map(|sg| &sg.frames).collect();

        if all_frames.is_empty() {
            continue;
        }

        // Build exposure groups
        let exposure_groups = build_exposure_groups(&all_frames);

        // Get representative frame for equipment info
        let rep_frame = all_frames.first().unwrap();

        // Calculate average temperature
        let temps: Vec<f64> = all_frames.iter().filter_map(|f| f.ccd_temp).collect();
        let avg_temp = if !temps.is_empty() {
            Some(temps.iter().sum::<f64>() / temps.len() as f64)
        } else {
            None
        };

        // Build calibration details from first subgroup (representative)
        let (flat_info, dark_info, bias_info) = if let Some(subgroup) = group.subgroups.first() {
            (
                subgroup
                    .flat
                    .as_ref()
                    .map(|f| build_calibration_detail(conn, f)),
                subgroup
                    .dark
                    .as_ref()
                    .map(|d| build_calibration_detail(conn, d)),
                subgroup
                    .bias
                    .as_ref()
                    .map(|b| build_calibration_detail(conn, b)),
            )
        } else {
            (None, None, None)
        };

        // Build frame details for expandable list
        let frames = build_frame_details(conn, &all_frames, group.subgroups.first())?;

        // Get telescope from frame
        let telescope = get_frame_telescope(conn, rep_frame.frame_id)?;

        summaries.push(FilterGroupSummary {
            filter: group.filter.clone(),
            camera_type: group.camera_type.clone(),
            camera: rep_frame.instrume.clone(),
            telescope,
            gain: rep_frame.gain,
            offset: rep_frame.offset,
            binning: rep_frame.binning.clone(),
            avg_temp,
            exposure_groups,
            total_exposure: group.total_exposure,
            frame_count: group.total_frames,
            flat_info,
            dark_info,
            bias_info,
            frames,
        });
    }

    Ok(summaries)
}

/// Build exposure groups from frames (group by exposure time)
fn build_exposure_groups(frames: &[&ExportFrame]) -> Vec<ExposureGroup> {
    let mut exp_counts: HashMap<i64, i32> = HashMap::new();

    for frame in frames {
        if let Some(exptime) = frame.exptime {
            // Round to nearest second for grouping
            let rounded = (exptime * 10.0).round() as i64; // 0.1s precision
            *exp_counts.entry(rounded).or_insert(0) += 1;
        }
    }

    let mut groups: Vec<ExposureGroup> = exp_counts
        .into_iter()
        .map(|(rounded, count)| {
            let exptime = rounded as f64 / 10.0;
            ExposureGroup {
                exptime,
                count,
                total_seconds: exptime * count as f64,
            }
        })
        .collect();

    // Sort by exposure time (longest first)
    groups.sort_by(|a, b| b.exptime.partial_cmp(&a.exptime).unwrap());

    groups
}

/// Build calibration detail from CalibrationSetInfo
fn build_calibration_detail(conn: &Connection, info: &CalibrationSetInfo) -> CalibrationDetail {
    // Calculate average exptime
    let avg_exptime = if !info.frames.is_empty() {
        let exps: Vec<f64> = info.frames.iter().filter_map(|f| f.exptime).collect();
        if !exps.is_empty() {
            Some(exps.iter().sum::<f64>() / exps.len() as f64)
        } else {
            None
        }
    } else {
        None
    };

    // Calculate average temperature
    let avg_temp = if !info.frames.is_empty() {
        let temps: Vec<f64> = info.frames.iter().filter_map(|f| f.ccd_temp).collect();
        if !temps.is_empty() {
            Some(temps.iter().sum::<f64>() / temps.len() as f64)
        } else {
            None
        }
    } else {
        None
    };

    // Get date range
    let date_range = get_date_range_from_frames(&info.frames);

    // Build sub-calibration details (recursive)
    let mut sub_calibrations = Vec::new();
    if let Some(ref dark_flat) = info.dark_flat {
        sub_calibrations.push(build_calibration_detail(conn, dark_flat));
    }
    if let Some(ref dark) = info.dark {
        sub_calibrations.push(build_calibration_detail(conn, dark));
    }
    if let Some(ref bias) = info.bias {
        sub_calibrations.push(build_calibration_detail(conn, bias));
    }

    CalibrationDetail {
        set_id: info.set_id,
        calibration_type: info.imagetyp.clone(),
        frame_count: info.frame_count,
        avg_exptime,
        avg_temp,
        match_score: info.match_score.unwrap_or(0.0),
        date_range,
        warnings: info.warnings.clone(),
        sub_calibrations,
    }
}

/// Get date range from a list of frames
fn get_date_range_from_frames(frames: &[ExportFrame]) -> Option<(String, String)> {
    let dates: Vec<&String> = frames.iter().filter_map(|f| f.date_obs.as_ref()).collect();
    if dates.len() >= 2 {
        let mut sorted = dates.clone();
        sorted.sort();
        Some((sorted.first().unwrap().to_string(), sorted.last().unwrap().to_string()))
    } else if dates.len() == 1 {
        Some((dates[0].clone(), dates[0].clone()))
    } else {
        None
    }
}

/// Get telescope name for a frame
fn get_frame_telescope(conn: &Connection, frame_id: i64) -> Result<Option<String>> {
    let telescope: Option<String> = conn
        .query_row(
            "SELECT telescop FROM frames WHERE id = ?1",
            [frame_id],
            |row| row.get(0),
        )
        .ok()
        .flatten();
    Ok(telescope)
}

/// Build frame details for expandable list
fn build_frame_details(
    conn: &Connection,
    frames: &[&ExportFrame],
    subgroup: Option<&CalibrationSubgroup>,
) -> Result<Vec<FrameDetail>> {
    let mut details = Vec::new();

    // Build calibration chain string once
    let calibration_chain = build_calibration_chain_string(subgroup);

    for frame in frames {
        // Get file size if available
        let file_size: Option<u64> = conn
            .query_row(
                "SELECT size FROM files WHERE id = ?1",
                [frame.file_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten()
            .map(|s| s as u64);

        details.push(FrameDetail {
            frame_id: frame.frame_id,
            filename: frame.filename.clone(),
            file_path: frame.file_path.clone(),
            date_obs: frame.date_obs.clone(),
            exptime: frame.exptime,
            temp: frame.ccd_temp,
            gain: frame.gain,
            offset: frame.offset,
            calibration_chain: calibration_chain.clone(),
            file_size,
        });
    }

    // Sort by date
    details.sort_by(|a, b| a.date_obs.cmp(&b.date_obs));

    Ok(details)
}

/// Build calibration chain string (e.g., "Flat #12 → Dark #8 → Bias #3")
fn build_calibration_chain_string(subgroup: Option<&CalibrationSubgroup>) -> String {
    let mut parts = Vec::new();

    if let Some(sg) = subgroup {
        if let Some(ref flat) = sg.flat {
            parts.push(format!("Flat #{}", flat.set_id));
        }
        if let Some(ref dark) = sg.dark {
            parts.push(format!("Dark #{}", dark.set_id));
        }
        if let Some(ref bias) = sg.bias {
            parts.push(format!("Bias #{}", bias.set_id));
        }
    }

    if parts.is_empty() {
        "No calibration".to_string()
    } else {
        parts.join(" → ")
    }
}

/// Build folder preview structure matching the new WBPP hierarchy
///
/// The preview mirrors the actual export: BIAS → DARKS → FLAT → lights nesting,
/// with missing calibration levels collapsed.
fn build_folder_preview(export_data: &ExportData, _config: &WbppExportConfig) -> Result<FolderPreview> {
    use crate::export::models::{sanitize_display_folder_name, sanitize_folder_name};

    let root_name = sanitize_display_folder_name(&export_data.frame_set_name);

    let mut root_children: Vec<FolderNode> = Vec::new();
    let mut total_files = 0;

    // Group by camera
    let mut cameras_map: HashMap<String, Vec<(&ExportGroup, &CalibrationSubgroup)>> = HashMap::new();
    for group in &export_data.groups {
        for subgroup in &group.subgroups {
            let camera_name = subgroup
                .frames
                .first()
                .and_then(|f| f.instrume.clone())
                .unwrap_or_else(|| "unknown".to_string());
            cameras_map.entry(camera_name).or_default().push((group, subgroup));
        }
    }

    for (camera, group_subgroups) in &cameras_map {
        let camera_folder_name = format!("camera_{}", sanitize_folder_name(camera));

        // Track organized set IDs to avoid counting duplicates across subgroups
        let mut counted_sets: HashSet<i64> = HashSet::new();

        // Build the camera's children by processing each subgroup
        // Each subgroup creates its own nested hierarchy path
        let mut camera_children: Vec<FolderNode> = Vec::new();

        for (_group, subgroup) in group_subgroups {
            // Resolve calibration sets (same logic as file_organizer)
            let bias: Option<&CalibrationSetInfo> = subgroup
                .bias
                .as_ref()
                .or_else(|| subgroup.dark.as_ref().and_then(|d| d.bias.as_deref()));
            let dark: Option<&CalibrationSetInfo> = subgroup.dark.as_ref();
            let flat: Option<&CalibrationSetInfo> = subgroup.flat.as_ref();
            let dark_flat: Option<&CalibrationSetInfo> =
                flat.and_then(|f| f.dark_flat.as_deref());
            let flat_dark: Option<&CalibrationSetInfo> =
                flat.and_then(|f| f.dark.as_deref());
            let flat_bias: Option<&CalibrationSetInfo> =
                flat.and_then(|f| f.bias.as_deref());

            // Build nested tree from outermost to innermost
            // We construct the tree bottom-up, then attach it to camera_children

            // Lights node (innermost)
            let lights_count = subgroup.frames.len() as i32;
            let example_light = subgroup
                .frames
                .first()
                .map(|f| f.filename.clone())
                .unwrap_or_else(|| "light_001.fits".to_string());
            let mut lights_children = vec![make_file_node(&example_light)];
            if lights_count > 1 {
                lights_children.push(make_ellipsis_node("light", lights_count));
            }
            total_files += lights_count;

            let lights_node = FolderNode {
                name: "lights".to_string(),
                node_type: FolderNodeType::Folder,
                file_count: Some(lights_count),
                description: Some(format!("← {} lights", lights_count)),
                children: lights_children,
            };

            // Current innermost content starts with lights
            let mut innermost_content = vec![lights_node];

            // FLAT level
            if let Some(flat_info) = flat {
                let flat_folder = format!("FLAT_{}", flat_info.set_id);
                let mut flat_children: Vec<FolderNode> = Vec::new();

                if counted_sets.insert(flat_info.set_id) {
                    let fc = flat_info.frame_count;
                    if fc > 0 {
                        let example = flat_info.frames.first().map(|f| f.filename.clone())
                            .unwrap_or_else(|| "flat_001.fits".to_string());
                        flat_children.push(make_file_node(&example));
                        if fc > 1 {
                            flat_children.push(make_ellipsis_node("flat", fc));
                        }
                        total_files += fc;
                    }
                }

                flat_children.extend(innermost_content);

                innermost_content = vec![FolderNode {
                    name: flat_folder,
                    node_type: FolderNodeType::Folder,
                    file_count: None,
                    description: Some(format!("← {} flats", flat_info.frame_count)),
                    children: flat_children,
                }];
            }

            // DARKS level
            let has_darks = dark.is_some() || dark_flat.is_some() || flat_dark.is_some();
            if has_darks {
                let darks_set_id = dark
                    .map(|d| d.set_id)
                    .or_else(|| flat_dark.map(|d| d.set_id))
                    .or_else(|| dark_flat.map(|df| df.set_id))
                    .unwrap_or(0);
                let darks_folder = format!("DARKS_{}", darks_set_id);
                let mut darks_children: Vec<FolderNode> = Vec::new();

                // Dark frames
                if let Some(dark_info) = dark {
                    if counted_sets.insert(dark_info.set_id) {
                        let dc = dark_info.frame_count;
                        if dc > 0 {
                            let example = dark_info.frames.first().map(|f| f.filename.clone())
                                .unwrap_or_else(|| "dark_001.fits".to_string());
                            darks_children.push(make_file_node(&example));
                            if dc > 1 {
                                darks_children.push(make_ellipsis_node("dark", dc));
                            }
                            total_files += dc;
                        }
                    }
                }

                // Flat's own dark
                if let Some(fd) = flat_dark {
                    if counted_sets.insert(fd.set_id) {
                        let fdc = fd.frame_count;
                        if fdc > 0 {
                            let example = fd.frames.first().map(|f| f.filename.clone())
                                .unwrap_or_else(|| "dark_001.fits".to_string());
                            darks_children.push(make_file_node(&example));
                            if fdc > 1 {
                                darks_children.push(make_ellipsis_node("dark", fdc));
                            }
                            total_files += fdc;
                        }
                    }
                }

                // Darkflat frames
                if let Some(df_info) = dark_flat {
                    if counted_sets.insert(df_info.set_id) {
                        let dfc = df_info.frame_count;
                        if dfc > 0 {
                            let example = df_info.frames.first().map(|f| f.filename.clone())
                                .unwrap_or_else(|| "darkflat_001.fits".to_string());
                            darks_children.push(make_file_node(&example));
                            if dfc > 1 {
                                darks_children.push(make_ellipsis_node("darkflat", dfc));
                            }
                            total_files += dfc;
                        }
                    }
                }

                darks_children.extend(innermost_content);

                let mut darks_desc_parts = Vec::new();
                if let Some(d) = dark { darks_desc_parts.push(format!("{} darks", d.frame_count)); }
                if let Some(df) = dark_flat { darks_desc_parts.push(format!("{} darkflats", df.frame_count)); }

                innermost_content = vec![FolderNode {
                    name: darks_folder,
                    node_type: FolderNodeType::Folder,
                    file_count: None,
                    description: Some(format!("← {}", darks_desc_parts.join(", "))),
                    children: darks_children,
                }];
            }

            // BIAS level
            let effective_bias = bias.or(flat_bias);
            if let Some(bias_info) = effective_bias {
                let bias_folder = format!("BIAS_{}", bias_info.set_id);
                let mut bias_children: Vec<FolderNode> = Vec::new();

                if counted_sets.insert(bias_info.set_id) {
                    let bc = bias_info.frame_count;
                    if bc > 0 {
                        let example = bias_info.frames.first().map(|f| f.filename.clone())
                            .unwrap_or_else(|| "bias_001.fits".to_string());
                        bias_children.push(make_file_node(&example));
                        if bc > 1 {
                            bias_children.push(make_ellipsis_node("bias", bc));
                        }
                        total_files += bc;
                    }
                }

                // Also count flat's own bias if different
                if let (Some(fb), Some(b)) = (flat_bias, bias) {
                    if fb.set_id != b.set_id && counted_sets.insert(fb.set_id) {
                        let fbc = fb.frame_count;
                        if fbc > 0 {
                            total_files += fbc;
                        }
                    }
                }

                bias_children.extend(innermost_content);

                innermost_content = vec![FolderNode {
                    name: bias_folder,
                    node_type: FolderNodeType::Folder,
                    file_count: None,
                    description: Some(format!("← {} bias", bias_info.frame_count)),
                    children: bias_children,
                }];
            }

            // Add the built hierarchy to camera children
            // Check if we already have a node with the same name to merge subgroups
            // that share the same calibration path into one tree
            for node in innermost_content {
                if let Some(existing) = camera_children.iter_mut().find(|n| n.name == node.name) {
                    // Merge children into existing node
                    merge_folder_children(existing, node);
                } else {
                    camera_children.push(node);
                }
            }
        }

        root_children.push(FolderNode {
            name: camera_folder_name,
            node_type: FolderNodeType::Folder,
            file_count: None,
            description: None,
            children: camera_children,
        });
    }

    Ok(FolderPreview {
        root_name,
        structure: root_children,
        total_files,
        estimated_size: format_bytes_human(0), // Will be calculated separately
    })
}

/// Create a file node for folder preview
fn make_file_node(filename: &str) -> FolderNode {
    FolderNode {
        name: filename.to_string(),
        node_type: FolderNodeType::File,
        file_count: None,
        description: None,
        children: vec![],
    }
}

/// Create an ellipsis node showing "... N more X frames"
fn make_ellipsis_node(frame_type: &str, total_count: i32) -> FolderNode {
    FolderNode {
        name: format!("... {} more {} frames", total_count - 1, frame_type),
        node_type: FolderNodeType::Ellipsis,
        file_count: Some(total_count),
        description: None,
        children: vec![],
    }
}

/// Merge children from source node into target node (for combining subgroups)
fn merge_folder_children(target: &mut FolderNode, source: FolderNode) {
    for child in source.children {
        if child.node_type == FolderNodeType::Folder {
            // Try to merge with existing folder of same name
            if let Some(existing) = target.children.iter_mut().find(|n| n.name == child.name && n.node_type == FolderNodeType::Folder) {
                merge_folder_children(existing, child);
            } else {
                target.children.push(child);
            }
        }
        // Skip file/ellipsis nodes during merge to avoid duplicates
    }
}

/// Build detailed warnings from export data
fn build_detailed_warnings(
    export_data: &ExportData,
    filter_groups: &[FilterGroupSummary],
) -> Result<Vec<DetailedWarning>> {
    let mut warnings = Vec::new();

    for group in filter_groups {
        let filter_name = group.filter.clone().unwrap_or_else(|| "Unfiltered".to_string());

        // Check temperature mismatches
        if let Some(ref dark_info) = group.dark_info {
            if let (Some(light_temp), Some(dark_temp)) = (group.avg_temp, dark_info.avg_temp) {
                let delta = (light_temp - dark_temp).abs();
                if delta > 2.0 {
                    warnings.push(DetailedWarning {
                        warning_type: WarningType::TemperatureMismatch,
                        severity: if delta > 5.0 {
                            WarningSeverity::Error
                        } else {
                            WarningSeverity::Warning
                        },
                        title: format!("Temperature Mismatch: Dark Set #{}", dark_info.set_id),
                        description: format!(
                            "Dark frames and light frames have different temperatures"
                        ),
                        set_id: Some(dark_info.set_id),
                        filter: Some(filter_name.clone()),
                        actual_value: Some(format!("{:.1}°C", dark_temp)),
                        expected_value: Some(format!("{:.1}°C", light_temp)),
                        delta: Some(format!("{:.1}°C", delta)),
                        recommendation: Some(
                            "For best results, darks should match light frame temperature within 1°C"
                                .to_string(),
                        ),
                    });
                }
            }
        }

        // Check for missing calibrations
        if group.flat_info.is_none() {
            warnings.push(DetailedWarning {
                warning_type: WarningType::MissingCalibration,
                severity: WarningSeverity::Warning,
                title: format!("Missing Flat Calibration: {}", filter_name),
                description: "No flat frames are linked to this filter's light frames".to_string(),
                set_id: None,
                filter: Some(filter_name.clone()),
                actual_value: None,
                expected_value: None,
                delta: None,
                recommendation: Some(
                    "Add flat frames for this filter to improve image quality".to_string(),
                ),
            });
        }

        if group.dark_info.is_none() {
            warnings.push(DetailedWarning {
                warning_type: WarningType::MissingCalibration,
                severity: WarningSeverity::Warning,
                title: format!("Missing Dark Calibration: {}", filter_name),
                description: "No dark frames are linked to this filter's light frames".to_string(),
                set_id: None,
                filter: Some(filter_name.clone()),
                actual_value: None,
                expected_value: None,
                delta: None,
                recommendation: Some(
                    "Add matching dark frames to remove thermal noise".to_string(),
                ),
            });
        }

        // Check calibration age
        if let Some(ref flat_info) = group.flat_info {
            if !flat_info.warnings.is_empty() {
                for warning_text in &flat_info.warnings {
                    if warning_text.contains("age") || warning_text.contains("old") {
                        warnings.push(DetailedWarning {
                            warning_type: WarningType::CalibrationAge,
                            severity: WarningSeverity::Info,
                            title: format!("Calibration Age: Flat Set #{}", flat_info.set_id),
                            description: warning_text.clone(),
                            set_id: Some(flat_info.set_id),
                            filter: Some(filter_name.clone()),
                            actual_value: None,
                            expected_value: None,
                            delta: None,
                            recommendation: Some(
                                "Consider using flats from the same imaging session".to_string(),
                            ),
                        });
                    }
                }
            }
        }
    }

    // Add warnings from the calibration summary
    for warning in &export_data.calibration_summary.warnings {
        // Only add if not already covered above
        let already_covered = warnings
            .iter()
            .any(|w| warning.contains(&w.title) || w.description.contains(warning));
        if !already_covered {
            warnings.push(DetailedWarning {
                warning_type: WarningType::General,
                severity: WarningSeverity::Info,
                title: "Calibration Warning".to_string(),
                description: warning.clone(),
                set_id: None,
                filter: None,
                actual_value: None,
                expected_value: None,
                delta: None,
                recommendation: None,
            });
        }
    }

    Ok(warnings)
}

/// Calculate total file count for export
fn calculate_total_files(export_data: &ExportData) -> i32 {
    let mut total = 0;

    // Light frames
    total += export_data.total_light_frames;

    // Calibration frames
    total += export_data.calibration_summary.flat_count;
    total += export_data.calibration_summary.dark_count;
    total += export_data.calibration_summary.bias_count;
    total += export_data.calibration_summary.dark_flat_count;

    total
}

/// Estimate total size of export in bytes
fn estimate_total_size(conn: &Connection, export_data: &ExportData) -> Result<u64> {
    let mut total_size: u64 = 0;

    // Get average file size from light frames in this set
    let avg_size: Option<i64> = conn
        .query_row(
            "SELECT AVG(fi.size)
             FROM files fi
             JOIN frames f ON fi.id = f.file_id
             JOIN session_members sm ON f.id = sm.frame_id
             JOIN sessions s ON sm.session_id = s.id
             JOIN imaging_nights n ON s.imaging_night_id = n.id
             WHERE n.frames_set_id = ?1 AND fi.size IS NOT NULL",
            [export_data.frame_set_id],
            |row| row.get(0),
        )
        .ok()
        .flatten();

    let avg_size = avg_size.unwrap_or(50_000_000) as u64; // Default 50MB

    // Estimate total
    total_size += export_data.total_light_frames as u64 * avg_size;
    total_size += export_data.calibration_summary.flat_count as u64 * avg_size;
    total_size += export_data.calibration_summary.dark_count as u64 * avg_size;
    total_size += export_data.calibration_summary.bias_count as u64 * avg_size;
    total_size += export_data.calibration_summary.dark_flat_count as u64 * avg_size;

    Ok(total_size)
}

/// Format bytes to human-readable string
fn format_bytes_human(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if bytes >= TB {
        format!("{:.1} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_filter_sorting() {
        let mut filters: Vec<(Option<String>, Vec<i64>)> = vec![
            (Some("R".to_string()), vec![]),
            (None, vec![]),
            (Some("Ha".to_string()), vec![]),
            (Some("G".to_string()), vec![]),
        ];

        filters.sort_by(|a, b| match (&a.0, &b.0) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (Some(a), Some(b)) => a.cmp(b),
        });

        assert_eq!(filters[0].0, None);
        assert_eq!(filters[1].0, Some("G".to_string()));
        assert_eq!(filters[2].0, Some("Ha".to_string()));
        assert_eq!(filters[3].0, Some("R".to_string()));
    }

    #[test]
    fn test_subgroup_key_generation() {
        use super::FrameCalibrationLinks;

        let links = FrameCalibrationLinks {
            frame_id: 1,
            flat_id: Some(5),
            dark_id: Some(3),
            bias_id: Some(1),
        };
        assert_eq!(links.subgroup_key(), "f5_d3_b1");

        let links_partial = FrameCalibrationLinks {
            frame_id: 2,
            flat_id: Some(5),
            dark_id: None,
            bias_id: Some(1),
        };
        assert_eq!(links_partial.subgroup_key(), "f5_dnone_b1");

        let links_none = FrameCalibrationLinks {
            frame_id: 3,
            flat_id: None,
            dark_id: None,
            bias_id: None,
        };
        assert_eq!(links_none.subgroup_key(), "fnone_dnone_bnone");
    }
}

/// Export-mode transform (spec §12.2) against a seeded in-memory catalog.
#[cfg(test)]
mod export_mode_tests {
    use super::{
        apply_export_mode, collect_export_data, export_file_counts, raw_sets_without_master,
        resolve_export_mode,
    };
    use crate::db::light_calibrations::{
        upsert_light_calibration, LightCalRow, LIGHT_CAL_ENGINE_VERSION,
    };
    use crate::db::schema::init_db;
    use crate::export::models::{ExportMode, WbppExportConfig};
    use rusqlite::{params, Connection};

    /// Regression: the export mode used to travel only via the persisted
    /// `WbppExportConfig`, so a stale/unloaded config on the frontend silently
    /// exported the wrong mode. An explicit per-invocation override must win
    /// over a differing persisted mode; `None` still falls back to the config.
    #[test]
    fn explicit_export_mode_overrides_persisted_config() {
        let config = WbppExportConfig {
            export_mode: ExportMode::RawWithCalibrationSets,
            ..WbppExportConfig::default()
        };

        // Explicit override wins over a differing persisted mode.
        assert_eq!(
            resolve_export_mode(Some(ExportMode::CalibratedLights), &config),
            ExportMode::CalibratedLights,
        );
        assert_eq!(
            resolve_export_mode(Some(ExportMode::RawWithMasters), &config),
            ExportMode::RawWithMasters,
        );

        // None falls back to the persisted config (historical behavior).
        assert_eq!(
            resolve_export_mode(None, &config),
            ExportMode::RawWithCalibrationSets,
        );
    }

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    /// Frame set + one imaging night + one session; returns `session_id`.
    fn seed_frame_set(conn: &Connection, fs_id: i64) -> i64 {
        conn.execute(
            "INSERT INTO frames_set (id, name) VALUES (?1, ?2)",
            params![fs_id, format!("Obj {fs_id}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
             VALUES (?1, '2026-07-05T20:00:00Z', '2026-07-05T23:00:00Z')",
            params![fs_id],
        )
        .unwrap();
        let night_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'TestCam')",
            params![night_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn seed_light(conn: &Connection, frame_id: i64, session_id: i64, filter: Option<&str>) {
        let file_id = frame_id + 2_000_000;
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
            params![
                file_id,
                format!("/test/light_{frame_id}.fits"),
                format!("light_{frame_id}.fits")
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, instrume, object, date_obs, filter)
             VALUES (?1, ?2, 'Light', 'TestCam', 'M31', '2026-07-05T20:30:00Z', ?3)",
            params![frame_id, file_id, filter],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            params![session_id, frame_id],
        )
        .unwrap();
    }

    /// A raw (non-master) calibration set with `n` member frames on disk.
    fn seed_raw_set(conn: &Connection, set_id: i64, imagetyp: &str, n: i64) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (?1, ?2, '2026-07-05', 0)",
            params![set_id, imagetyp],
        )
        .unwrap();
        for i in 0..n {
            let file_id = set_id * 100 + i + 5_000_000;
            let frame_id = set_id * 100 + i + 6_000_000;
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
                params![
                    file_id,
                    format!("/raw/{imagetyp}_{set_id}_{i}.fits"),
                    format!("{imagetyp}_{set_id}_{i}.fits")
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp) VALUES (?1, ?2, ?3)",
                params![frame_id, file_id, imagetyp],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                params![set_id, frame_id],
            )
            .unwrap();
        }
        set_id
    }

    /// A MASTER calibration set (`is_master_library = 1`) with one member file.
    fn seed_master_set(conn: &Connection, set_id: i64, imagetyp: &str) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (?1, ?2, '2026-07-05', 1)",
            params![set_id, imagetyp],
        )
        .unwrap();
        let file_id = set_id + 3_000_000;
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
            params![
                file_id,
                format!("/lib/master_{set_id}.fits"),
                format!("master_{set_id}.fits")
            ],
        )
        .unwrap();
        let frame_id = set_id + 4_000_000;
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, is_master) VALUES (?1, ?2, ?3, 1)",
            params![frame_id, file_id, imagetyp],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            params![set_id, frame_id],
        )
        .unwrap();
        set_id
    }

    fn add_link(conn: &Connection, frame_id: i64, set_id: i64, cal_type: &str) {
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (?1, 'frame', ?2, ?3, '2026-07-05T00:00:00Z')",
            params![frame_id, set_id, cal_type],
        )
        .unwrap();
    }

    fn track_row(frame_id: i64, output_path: &str, dark_set_id: Option<i64>) -> LightCalRow {
        LightCalRow {
            id: 0,
            frame_id: Some(frame_id),
            source_uuid: None,
            source_filename: Some(format!("light_{frame_id}.fits")),
            output_path: output_path.to_string(),
            dark_set_id,
            flat_set_id: None,
            bias_set_id: None,
            calstat: "BD".to_string(),
            flat_norm_applied: false,
            flat_norm_mode: "centralThird".to_string(),
            output_hash: "hash".to_string(),
            engine_version: LIGHT_CAL_ENGINE_VERSION,
            created_at: "2026-07-05T21:00:00Z".to_string(),
            cal_params: "{}".to_string(),
            cfa_scaling_applied: None,
        }
    }

    /// Regression pin: the default mode never touches the collected data.
    #[test]
    fn default_mode_is_bit_for_bit_noop() {
        let conn = mem();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 10, session, Some("Ha"));
        let dark = seed_raw_set(&conn, 100, "Dark", 2);
        add_link(&conn, 10, dark, "Dark");

        let mut data = collect_export_data(&conn, 1).unwrap();
        let before = serde_json::to_value(&data).unwrap();
        let warnings =
            apply_export_mode(&conn, &mut data, ExportMode::RawWithCalibrationSets).unwrap();
        assert!(warnings.is_empty(), "default mode emits no warnings");
        assert_eq!(
            serde_json::to_value(&data).unwrap(),
            before,
            "default mode must leave ExportData unchanged"
        );
    }

    /// CalibratedLights swaps the raw light for its artifact and drops calibration.
    #[test]
    fn calibrated_lights_substitutes_artifact_paths() {
        let conn = mem();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 10, session, Some("Ha"));
        let dark = seed_raw_set(&conn, 100, "Dark", 2);
        add_link(&conn, 10, dark, "Dark");
        upsert_light_calibration(
            &conn,
            &track_row(10, "/lib/M31/TestCam/2026-07-05/c_light_10.fits", Some(dark)),
        )
        .unwrap();

        let mut data = collect_export_data(&conn, 1).unwrap();
        let warnings =
            apply_export_mode(&conn, &mut data, ExportMode::CalibratedLights).unwrap();
        assert!(warnings.is_empty());

        let sg = &data.groups[0].subgroups[0];
        assert!(sg.flat.is_none() && sg.dark.is_none() && sg.bias.is_none(), "no calibration frames");
        assert_eq!(sg.frames.len(), 1);
        assert_eq!(
            sg.frames[0].file_path,
            "/lib/M31/TestCam/2026-07-05/c_light_10.fits",
            "light source is the calibrated artifact"
        );
        assert_eq!(sg.frames[0].filename, "c_light_10.fits");
    }

    /// The strict gate errors (never a partial silent export) when a light has no
    /// calibrated output.
    #[test]
    fn calibrated_lights_gate_errors_on_missing_output() {
        let conn = mem();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 10, session, Some("Ha")); // no tracking row

        let mut data = collect_export_data(&conn, 1).unwrap();
        let err = apply_export_mode(&conn, &mut data, ExportMode::CalibratedLights).unwrap_err();
        assert!(
            err.to_string().contains("lack a fresh calibrated output"),
            "gate message, got: {err}"
        );
    }

    /// LightsOnly drops every calibration node and never touches light paths.
    #[test]
    fn lights_only_drops_calibration_and_keeps_light_paths() {
        let conn = mem();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 10, session, Some("Ha"));
        let dark = seed_raw_set(&conn, 100, "Dark", 2);
        let flat = seed_master_set(&conn, 200, "Flat");
        add_link(&conn, 10, dark, "Dark");
        add_link(&conn, 10, flat, "Flat");

        let mut data = collect_export_data(&conn, 1).unwrap();
        let warnings = apply_export_mode(&conn, &mut data, ExportMode::LightsOnly).unwrap();
        assert!(warnings.is_empty());

        let sg = &data.groups[0].subgroups[0];
        assert!(sg.flat.is_none() && sg.dark.is_none() && sg.bias.is_none());
        assert_eq!(sg.frames.len(), 1);
        assert_eq!(
            sg.frames[0].file_path, "/test/light_10.fits",
            "raw light path untouched"
        );
        let placements = crate::export::file_organizer::compute_wbpp_placements(&data);
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].rel_dir, "camera_testcam/lights");
    }

    /// raw_sets_without_master lists only raw sets that have frames, once each.
    #[test]
    fn raw_sets_without_master_counts_raw_sets_with_frames_once() {
        let conn = mem();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 10, session, Some("Ha"));
        seed_light(&conn, 11, session, Some("Ha"));
        let dark = seed_raw_set(&conn, 100, "Dark", 2);
        let empty = seed_raw_set(&conn, 101, "Bias", 0);
        let flat = seed_master_set(&conn, 200, "Flat");
        for f in [10, 11] {
            add_link(&conn, f, dark, "Dark");
            add_link(&conn, f, empty, "Bias");
            add_link(&conn, f, flat, "Flat");
        }
        let data = collect_export_data(&conn, 1).unwrap();
        assert_eq!(raw_sets_without_master(&conn, &data).unwrap(), vec![100]);
    }

    /// Strict raw+masters: a raw set with frames is an error, never an omission.
    #[test]
    fn raw_with_masters_errors_on_raw_set_with_frames() {
        let conn = mem();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 10, session, Some("Ha"));
        let dark = seed_raw_set(&conn, 100, "Dark", 2);
        add_link(&conn, 10, dark, "Dark");
        let mut data = collect_export_data(&conn, 1).unwrap();
        let err = apply_export_mode(&conn, &mut data, ExportMode::RawWithMasters).unwrap_err();
        assert!(err.to_string().contains("no master"), "got: {err}");
    }

    /// Strict raw+masters passes untouched when every linked set is a master.
    #[test]
    fn raw_with_masters_is_noop_when_all_masters() {
        let conn = mem();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 10, session, Some("Ha"));
        let flat = seed_master_set(&conn, 200, "Flat");
        add_link(&conn, 10, flat, "Flat");
        let mut data = collect_export_data(&conn, 1).unwrap();
        let before = serde_json::to_value(&data).unwrap();
        let warnings = apply_export_mode(&conn, &mut data, ExportMode::RawWithMasters).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(serde_json::to_value(&data).unwrap(), before);
    }

    /// Per-mode file counts equal the placements each mode would produce.
    #[test]
    fn export_file_counts_match_placements() {
        let conn = mem();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 10, session, Some("Ha"));
        seed_light(&conn, 11, session, Some("Ha"));
        let dark = seed_raw_set(&conn, 100, "Dark", 3);
        let flat = seed_master_set(&conn, 200, "Flat");
        for f in [10, 11] {
            add_link(&conn, f, dark, "Dark");
            add_link(&conn, f, flat, "Flat");
        }
        let data = collect_export_data(&conn, 1).unwrap();
        let counts = export_file_counts(&conn, &data).unwrap();
        assert_eq!(counts.lights_only, 2);
        assert_eq!(counts.raw_with_calibration_sets, 2 + 3 + 1);
        assert_eq!(
            counts.raw_with_masters,
            2 + 1,
            "raw dark set contributes nothing"
        );
        assert_eq!(counts.calibrated_lights, 2);
    }
}
