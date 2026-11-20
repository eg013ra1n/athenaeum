//! Which light groups have darks, flats and bias linked (MCP plan, Task 3, R4). One line per night / camera /
//! filter group of the hierarchy of each frame set the scope's lights belong to, read through the app's own
//! calibration hierarchy; the reason is the export readiness sentence of the set, said only when the app states one.
//!
//! The summary is paged by frame set (the hierarchy and the readiness of a set cost a walk of its export tree).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::params_from_iter;
use serde::Serialize;

use super::{is_raw_light, none_of, ScopedFrame};
use crate::api::ApiError;
use crate::services::ServiceContext;

/// Frame sets on one page of the calibration summary.
pub const FRAME_SETS_PAGE: u32 = 25;

/// Ids per statement (the statement binds one variable per id).
const ID_CHUNK: usize = 900;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationLine {
    pub frame_set: String,
    /// The scope's night of the group's lights (`YYYY-MM-DD`), or `first to last` when the group spans several.
    pub night: String,
    pub camera: String,
    /// The filter, with the exposure when the app splits the group by it (`Ha (300s)`).
    pub filter: String,
    /// The group's lights the scope lets through (a second copy of a frame the set already holds is not one).
    pub lights: usize,
    /// Every one of those lights has a dark / flat / bias linked.
    pub darks: bool,
    pub flats: bool,
    pub bias: bool,
    /// The set's export readiness sentence when the set is not ready; `None` where the app states none.
    pub reason: Option<String>,
}

/// The frame sets (id, name) the given frame ids belong to.
fn sets_of_lights(ctx: &ServiceContext, frame_ids: &[i64]) -> Result<BTreeMap<i64, String>, ApiError> {
    let db = crate::api::db(ctx)?;
    let conn = db.conn();
    let mut sets = BTreeMap::new();
    for chunk in frame_ids.chunks(ID_CHUNK) {
        let marks = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "SELECT DISTINCT fs.id, fs.name FROM session_members sm \
             JOIN sessions ss ON ss.id = sm.session_id \
             JOIN imaging_nights n ON n.id = ss.imaging_night_id \
             JOIN frames_set fs ON fs.id = n.frames_set_id \
             WHERE sm.frame_id IN ({marks})"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| {
            tracing::error!(error = %e, "calibration summary: frame set read failed");
            ApiError::Internal(format!("could not read the frame sets: {e}"))
        })?;
        let rows = stmt
            .query_map(params_from_iter(chunk.iter()), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)))
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(|e| {
                tracing::error!(error = %e, "calibration summary: frame set read failed");
                ApiError::Internal(format!("could not read the frame sets: {e}"))
            })?;
        for (id, name) in rows {
            let name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| none_of("name"));
            sets.insert(id, name);
        }
    }
    Ok(sets)
}

/// The sentence the app gives for a set whose lights cannot yet be calibrated for export, `None` when it is ready
/// (or when this build has no export readiness).
fn readiness_reason(ctx: &ServiceContext, set_id: i64) -> Result<Option<String>, ApiError> {
    #[cfg(feature = "render")]
    {
        let readiness = crate::api::lights::get_export_readiness(ctx, set_id)?;
        Ok(crate::api::lights::check_mode_ready(&readiness, crate::export::models::ExportMode::CalibratedLights).err())
    }
    #[cfg(not(feature = "render"))]
    {
        let _ = (ctx, set_id);
        Ok(None)
    }
}

fn nights_text(nights: &BTreeSet<String>) -> String {
    match (nights.first(), nights.last()) {
        (Some(a), Some(b)) if a == b => a.clone(),
        (Some(a), Some(b)) => format!("{a} to {b}"),
        _ => none_of("night"),
    }
}

/// The calibration lines of the frame sets the raw lights of `frames` belong to, one page of
/// [`FRAME_SETS_PAGE`] sets (zero-based `page`, sets by name), and the number of frame sets in all.
///
/// A group appears when the scope lets at least one of its lights through. No `ctx` connection is held across an
/// `api` call.
pub fn calibration_summary(
    ctx: &ServiceContext,
    frames: &[ScopedFrame],
    page: u32,
) -> Result<(Vec<CalibrationLine>, usize), ApiError> {
    let mut lights: HashMap<i64, &ScopedFrame> = HashMap::new();
    for f in frames {
        if let Some(frame) = f.file.frame.as_ref().filter(|x| is_raw_light(x)) {
            if let Some(id) = frame.id {
                lights.insert(id, f);
            }
        }
    }
    let ids: Vec<i64> = lights.keys().copied().collect();
    let sets = sets_of_lights(ctx, &ids)?;
    let total = sets.len();

    let mut ordered: Vec<(i64, String)> = sets.into_iter().collect();
    ordered.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    let start = (page as usize).saturating_mul(FRAME_SETS_PAGE as usize);

    let mut lines = Vec::new();
    for (set_id, set_name) in ordered.into_iter().skip(start).take(FRAME_SETS_PAGE as usize) {
        let hierarchy = crate::api::calibration::get_calibration_hierarchy_for_frame_set(ctx, set_id)?;
        let reason = readiness_reason(ctx, set_id)?;
        for date in &hierarchy.date_groups {
            for camera in &date.camera_groups {
                for group in &camera.filter_groups {
                    let mut nights = BTreeSet::new();
                    let (mut n, mut darks, mut flats, mut bias) = (0usize, 0usize, 0usize, 0usize);
                    for light in group.light_frames.iter().filter(|l| !l.duplicate_not_used) {
                        let Some(scoped) = lights.get(&light.frame_id) else { continue };
                        n += 1;
                        nights.insert(scoped.night.clone().unwrap_or_else(|| none_of("night")));
                        let status = &light.calibration_status;
                        darks += usize::from(status.has_darks);
                        flats += usize::from(status.has_flats);
                        bias += usize::from(status.has_bias);
                    }
                    if n == 0 {
                        continue;
                    }
                    let base = group.filter.clone().unwrap_or_else(|| none_of("filter"));
                    lines.push(CalibrationLine {
                        frame_set: set_name.clone(),
                        night: nights_text(&nights),
                        camera: if camera.instrume == "Unknown Camera" { none_of("camera") } else { camera.instrume.clone() },
                        filter: match group.exptime {
                            Some(e) => format!("{base} ({e}s)"),
                            None => base,
                        },
                        lights: n,
                        darks: darks == n,
                        flats: flats == n,
                        bias: bias == n,
                        reason: reason.clone(),
                    });
                }
            }
        }
    }
    Ok((lines, total))
}
