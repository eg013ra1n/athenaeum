//! Shared helpers for the manual-selection calibration endpoints.
//!
//! Both `athenaeum-tauri` and `athenaeum-web` expose
//! `get_calibration_sets_for_manual_selection` and
//! `get_subcalibration_sets_for_manual_selection`. Their bodies route through
//! `configurable_matcher::find_calibration_candidates` (the same engine
//! auto-link uses), and use these helpers to (1) build a synthetic `Frame`
//! from frame IDs / a calibration set id, and (2) load the engine's
//! `CalibrationCandidate` results back into the legacy `CalibrationSetWithScore`
//! response shape the React modals already consume.

use crate::calibration::finder::{CalibrationCandidate, CandidateMatchDetails};
use crate::models::{
    CalibrationSetDetail, CalibrationSetWithScore, Frame, ImageType, MatchDetails,
};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::collections::HashMap;

/// Build a synthetic `Frame` representing the average parameters of the
/// supplied light frames so the candidate engine sees the same shape it uses
/// for auto-link.
pub fn synthesize_frame_for_lights(conn: &Connection, frame_ids: &[i64]) -> Result<Frame> {
    if frame_ids.is_empty() {
        return Err(anyhow!("No frame IDs provided"));
    }

    let placeholders: Vec<String> = frame_ids.iter().map(|_| "?".to_string()).collect();
    let sql = format!(
        "SELECT instrume, binning, gain, offset, filter, ccd_temp, exptime, focallen, telescop, date_obs
         FROM frames WHERE id IN ({})",
        placeholders.join(", ")
    );

    let mut stmt = conn.prepare(&sql).context("preparing frame lookup")?;
    let params: Vec<&dyn rusqlite::ToSql> = frame_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
    let mut rows = stmt.query(params.as_slice()).context("querying frames")?;

    let mut instrumes: Vec<Option<String>> = Vec::new();
    let mut binnings: Vec<Option<String>> = Vec::new();
    let mut gains: Vec<Option<f64>> = Vec::new();
    let mut offsets: Vec<Option<f64>> = Vec::new();
    let mut filters: Vec<Option<String>> = Vec::new();
    let mut focallens: Vec<Option<f64>> = Vec::new();
    let mut telescops: Vec<Option<String>> = Vec::new();
    let mut temps: Vec<f64> = Vec::new();
    let mut exptimes: Vec<f64> = Vec::new();
    let mut earliest_date: Option<String> = None;

    while let Some(row) = rows.next().context("reading frame row")? {
        instrumes.push(row.get(0).ok());
        binnings.push(row.get(1).ok());
        gains.push(row.get(2).ok());
        offsets.push(row.get(3).ok());
        filters.push(row.get(4).ok());
        if let Ok(Some(t)) = row.get::<_, Option<f64>>(5) { temps.push(t); }
        if let Ok(Some(e)) = row.get::<_, Option<f64>>(6) { exptimes.push(e); }
        focallens.push(row.get(7).ok());
        telescops.push(row.get(8).ok());
        if let Ok(Some(d)) = row.get::<_, Option<String>>(9) {
            earliest_date = match earliest_date.take() {
                Some(prev) if prev <= d => Some(prev),
                _ => Some(d),
            };
        }
    }

    let avg_temp = if temps.is_empty() {
        None
    } else {
        Some(temps.iter().sum::<f64>() / temps.len() as f64)
    };
    let avg_exptime = if exptimes.is_empty() {
        None
    } else {
        Some(exptimes.iter().sum::<f64>() / exptimes.len() as f64)
    };
    let date_obs = earliest_date
        .as_ref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    Ok(Frame {
        id: None,
        file_id: 0,
        object: None,
        date_obs,
        telescop: most_common_option(&telescops),
        instrume: most_common_option(&instrumes),
        exptime: avg_exptime,
        filter: most_common_option(&filters),
        imagetyp: Some(ImageType::Light),
        is_master: false,
        gain: most_common_f64(&gains),
        offset: most_common_f64(&offsets),
        binning: most_common_option(&binnings),
        xbinning: None,
        ybinning: None,
        ccd_temp: avg_temp,
        set_temp: None,
        focallen: most_common_f64(&focallens),
        xpixsz: None,
        ypixsz: None,
        naxis1: None,
        naxis2: None,
        ra: None,
        dec: None,
        sitelat: None,
        lat_obs: None,
        sitelong: None,
        long_obs: None,
        objctra: None,
        objctdec: None,
        override_: false,
        swcreate: None,
        bayerpat: None,
        xbayroff: None,
        ybayroff: None,
        roworder: None,
        rotation: None,
        uuid: None,
        updated_at: None,
    })
}

/// Build a synthetic `Frame` from a calibration set's stored parameters and
/// determine which `source_type` (`flats` / `darks`) the candidate engine
/// should use when looking for sub-calibration.
pub fn synthesize_frame_for_set(conn: &Connection, set_id: i64) -> Result<(Frame, String)> {
    let mut stmt = conn.prepare(
        "SELECT imagetyp, instrume, binning, gain, offset, exptime, filter,
                ccd_temp, focallen, date_start
         FROM calibration_set WHERE id = ?1",
    ).context("preparing set lookup")?;

    let (imagetyp, instrume, binning, gain, offset, exptime, filter, ccd_temp, focallen, date_start): (
        String, Option<String>, Option<String>, Option<f64>, Option<f64>,
        Option<f64>, Option<String>, Option<f64>, Option<f64>, Option<String>,
    ) = stmt
        .query_row([set_id], |row| {
            Ok((
                row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?,
            ))
        })
        .with_context(|| format!("Calibration set {} not found", set_id))?;

    let source_type = match imagetyp.as_str() {
        "Flat" | "MasterFlat" => "flats",
        "Dark" | "MasterDark" => "darks",
        other => {
            return Err(anyhow!(
                "Sub-calibration is not defined for parent set imagetyp '{}'",
                other
            ))
        }
    }
    .to_string();

    let date_obs = date_start
        .as_ref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let frame = Frame {
        id: None,
        file_id: 0,
        object: None,
        date_obs,
        telescop: None,
        instrume,
        exptime,
        filter,
        imagetyp: ImageType::from_str(&imagetyp),
        is_master: false,
        gain,
        offset,
        binning,
        xbinning: None,
        ybinning: None,
        ccd_temp,
        set_temp: None,
        focallen,
        xpixsz: None,
        ypixsz: None,
        naxis1: None,
        naxis2: None,
        ra: None,
        dec: None,
        sitelat: None,
        lat_obs: None,
        sitelong: None,
        long_obs: None,
        objctra: None,
        objctdec: None,
        override_: false,
        swcreate: None,
        bayerpat: None,
        xbayroff: None,
        ybayroff: None,
        roworder: None,
        rotation: None,
        uuid: None,
        updated_at: None,
    };

    Ok((frame, source_type))
}

/// Map the engine's per-parameter `CandidateMatchDetails` into the legacy
/// `MatchDetails` shape consumed by the React modals.
pub fn match_details_from_candidate(details: &CandidateMatchDetails) -> MatchDetails {
    MatchDetails {
        instrume_match: details.instrume.matched,
        binning_match: details.binning.matched,
        gain_match: details.gain.matched,
        offset_match: details.offset.matched,
        exptime_match: details.exptime.matched,
        filter_match: details.filter.matched,
        temp_diff: details.temp_diff,
        date_diff_days: details.date_diff_days,
    }
}

/// Load a candidate's full `CalibrationSetDetail` via SQL and wrap it with the
/// engine's score and per-parameter details.
pub fn load_set_with_score(
    conn: &Connection,
    candidate: &CalibrationCandidate,
) -> Result<Option<CalibrationSetWithScore>> {
    let mut stmt = conn.prepare(
        "SELECT cs.id, cs.imagetyp, cs.exptime, cs.ccd_temp, cs.gain, cs.offset,
                cs.binning, cs.instrume, cs.filter, cs.date_start, cs.date_end,
                cs.temp_min, cs.temp_max, cs.frame_count, cs.focallen, cs.is_master_library,
                f.naxis1, f.naxis2, f.bayerpat, f.swcreate, f.xpixsz, fi.format,
                cs.uuid, cs.updated_at, cs.superseded_by_set_id
         FROM calibration_set cs
         LEFT JOIN calibration_set_frames csf ON csf.set_id = cs.id
         LEFT JOIN frames f ON f.id = csf.frame_id
         LEFT JOIN files fi ON fi.id = f.file_id
         WHERE cs.id = ?1
         GROUP BY cs.id",
    )?;

    let set_opt = stmt
        .query_row([candidate.set_id], |row| {
            let date_start: String = row.get::<_, Option<String>>(9)?.unwrap_or_default();
            let date_end: String = row.get::<_, Option<String>>(10)?.unwrap_or_default();
            let date_display = if date_start == date_end {
                date_start.chars().take(10).collect()
            } else {
                let s: String = date_start.chars().take(10).collect();
                let e: String = date_end.chars().take(10).collect();
                format!("{} - {}", s, e)
            };
            Ok(CalibrationSetDetail {
                id: row.get(0)?,
                imagetyp: ImageType::from_str(row.get::<_, String>(1)?.as_str())
                    .unwrap_or(ImageType::Flat),
                exptime: row.get(2)?,
                ccd_temp: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                gain: row.get(4)?,
                offset: row.get(5)?,
                binning: row.get(6)?,
                instrume: row.get(7)?,
                filter: row.get(8)?,
                date_start,
                date_end,
                temp_min: row.get::<_, Option<f64>>(11)?.unwrap_or(0.0),
                temp_max: row.get::<_, Option<f64>>(12)?.unwrap_or(0.0),
                frame_count: row.get::<_, Option<i64>>(13)?.unwrap_or(0),
                date_display,
                is_master: row.get::<_, i32>(15).unwrap_or(0) == 1,
                naxis1: row.get(16)?,
                naxis2: row.get(17)?,
                bayerpat: row.get(18)?,
                swcreate: row.get(19)?,
                xpixsz: row.get(20)?,
                format: row.get(21)?,
                focallen: row.get(14)?,
                uuid: row.get(22)?,
                updated_at: row.get(23)?,
                superseded_by_set_id: row.get(24)?,
            })
        })
        .ok();

    Ok(set_opt.map(|set| CalibrationSetWithScore {
        set,
        match_score: candidate.match_score,
        match_details: match_details_from_candidate(&candidate.details),
    }))
}

fn most_common_option(values: &[Option<String>]) -> Option<String> {
    let mut counts: HashMap<&String, usize> = HashMap::new();
    for v in values.iter().flatten() {
        *counts.entry(v).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(v, _)| v.clone())
}

fn most_common_f64(values: &[Option<f64>]) -> Option<f64> {
    let mut counts: HashMap<String, (f64, usize)> = HashMap::new();
    for v in values.iter().flatten() {
        let key = format!("{:.6}", v);
        let entry = counts.entry(key).or_insert((*v, 0));
        entry.1 += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, (_, n))| *n)
        .map(|(_, (v, _))| v)
}
