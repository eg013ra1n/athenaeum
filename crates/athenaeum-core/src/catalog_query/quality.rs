//! How good the frames are: one stored measurement over the raw lights of a scope, as a range, a median and the
//! outliers named by file (MCP plan, Task 3). A value that was not measured is said to be not measured and never
//! estimated; a row written by an earlier measurement version is counted apart and never enters the figures.
//!
//! Partition of the raw lights asked about: `measured + not_measured + older_version + sum(refused)`. Only raw
//! lights are asked (analysis measures nothing else), and a frame flagged `is_master` is never one.

// A build without `render` has no measurement to read: the helpers below are used by the `render` path only.
#![cfg_attr(not(feature = "render"), allow(dead_code, unused_imports))]

use std::collections::HashMap;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::{file_line, is_raw_light, FileLine, ScopedFrame};
use crate::api::ApiError;
use crate::models::{FrameAnalysis, FrameVerdict};

/// Outliers listed on each side.
pub const OUTLIERS_SHOWN: usize = 5;

/// `get_frame_analyses_by_ids` binds one `?` per id with no chunking; the ids go in chunks of this size.
const ANALYSIS_CHUNK: usize = 900;

/// The scale that turns a median absolute deviation into a standard deviation for normal data.
const MAD_SIGMA: f64 = 1.4826;
const OUTLIER_SIGMAS: f64 = 3.0;

const RUN_ANALYSIS: &str = "run Analysis on these frames in Athenaeum";
const SOLVE_THEN_ANALYSE: &str = "plate-solve these frames in Athenaeum, then run Analysis";

/// A stored measurement a question can ask about (a tool argument).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(crate = "rmcp::schemars")]
pub enum QualityMetric {
    Fwhm,
    Hfr,
    Eccentricity,
    Stars,
    Snr,
    FrameSnr,
    Background,
    Noise,
    QualityScore,
    TransparencyLoss,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualitySummary {
    pub metric: QualityMetric,
    /// Frames with a value at the current measurement version.
    pub measured: usize,
    /// Frames with no analysis row, or a current row without this value.
    pub not_measured: usize,
    /// Frames whose analysis was written by an earlier measurement version.
    pub older_version: usize,
    /// Frames the measurement refused (a frame-level verdict), by reason.
    pub refused: Vec<(String, usize)>,
    pub min: Option<f64>,
    pub median: Option<f64>,
    pub max: Option<f64>,
    /// Below the median by more than three robust standard deviations, worst first.
    pub low_outliers: Vec<(FileLine, f64)>,
    /// Above the median by more than three robust standard deviations, worst first.
    pub high_outliers: Vec<(FileLine, f64)>,
    /// Names the app action that would produce what is missing; `None` when nothing is missing.
    pub not_measured_reason: Option<String>,
}

fn verdict_label(v: FrameVerdict) -> &'static str {
    match v {
        FrameVerdict::Trailed => "stars trailed",
        FrameVerdict::Defocused => "stars defocused",
        FrameVerdict::NoStars => "no stars found",
    }
}

fn value_of(metric: QualityMetric, a: &FrameAnalysis) -> Option<f64> {
    let v = match metric {
        QualityMetric::Fwhm => Some(a.median_fwhm),
        QualityMetric::Hfr => Some(a.median_hfr),
        QualityMetric::Eccentricity => Some(a.median_eccentricity),
        QualityMetric::Stars => Some(a.stars_detected as f64),
        QualityMetric::Snr => Some(a.median_snr),
        QualityMetric::FrameSnr => Some(a.frame_snr),
        QualityMetric::Background => Some(a.background),
        QualityMetric::Noise => Some(a.noise),
        QualityMetric::QualityScore => a.quality_score,
        // Read from the transparency table, not from the analysis row.
        QualityMetric::TransparencyLoss => None,
    };
    v.filter(|x| x.is_finite())
}

/// The median of an already sorted, non-empty slice.
fn median_sorted(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

fn empty(metric: QualityMetric, not_measured: usize, reason: Option<String>) -> QualitySummary {
    QualitySummary {
        metric,
        measured: 0,
        not_measured,
        older_version: 0,
        refused: Vec::new(),
        min: None,
        median: None,
        max: None,
        low_outliers: Vec::new(),
        high_outliers: Vec::new(),
        not_measured_reason: reason,
    }
}

/// The analysis rows of `ids`, read in chunks the statement's variable limit allows.
fn analyses_of(conn: &Connection, ids: &[i64]) -> Result<HashMap<i64, FrameAnalysis>, ApiError> {
    let mut out = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(ANALYSIS_CHUNK) {
        let rows = crate::db::analysis::get_frame_analyses_by_ids(conn, chunk).map_err(|e| {
            tracing::error!(error = %e, "quality summary: analysis read failed");
            ApiError::Internal(format!("could not read the stored measurements: {e}"))
        })?;
        for row in rows {
            out.insert(row.frame_id, row);
        }
    }
    Ok(out)
}

/// One stored measurement over the raw lights of `frames`. `catalog` is the installed star catalogue's identity
/// (`analysis::transparency::installed_catalog_identity`), used by `TransparencyLoss` only; `conn` is the reader
/// connection (R11).
pub fn quality_summary(
    conn: &Connection,
    catalog: Option<&str>,
    frames: &[ScopedFrame],
    metric: QualityMetric,
) -> Result<QualitySummary, ApiError> {
    let lights: Vec<(&ScopedFrame, i64)> = frames
        .iter()
        .filter_map(|f| {
            let frame = f.file.frame.as_ref().filter(|x| is_raw_light(x))?;
            Some((f, frame.id?))
        })
        .collect();

    #[cfg(not(feature = "render"))]
    {
        let _ = (conn, catalog);
        let reason = if metric == QualityMetric::TransparencyLoss {
            "this build measures no transparency"
        } else {
            "this build measures no star quality"
        };
        return Ok(empty(metric, lights.len(), (!lights.is_empty()).then(|| reason.to_string())));
    }
    #[cfg(feature = "render")]
    {
        #[cfg(not(feature = "solver"))]
        if metric == QualityMetric::TransparencyLoss {
            let _ = (conn, catalog);
            return Ok(empty(
                metric,
                lights.len(),
                (!lights.is_empty()).then(|| "this build measures no transparency".to_string()),
            ));
        }
        summarise(conn, catalog, &lights, metric)
    }
}

#[cfg(feature = "render")]
fn summarise(
    conn: &Connection,
    catalog: Option<&str>,
    lights: &[(&ScopedFrame, i64)],
    metric: QualityMetric,
) -> Result<QualitySummary, ApiError> {
    let current = Some(crate::star_measure::MEASURE_VERSION);
    let ids: Vec<i64> = lights.iter().map(|(_, id)| *id).collect();
    let analyses = analyses_of(conn, &ids)?;

    // One transparency read for the whole summary (it chunks itself).
    #[cfg(feature = "solver")]
    let transparency = if metric == QualityMetric::TransparencyLoss {
        crate::analysis::transparency::transparency_for_frames(conn, catalog, &ids).map_err(|e| {
            tracing::error!(error = %e, "quality summary: transparency read failed");
            ApiError::Internal(format!("could not read the transparency: {e}"))
        })?
    } else {
        HashMap::new()
    };
    #[cfg(not(feature = "solver"))]
    let _ = catalog;

    let mut measured: Vec<(&ScopedFrame, f64)> = Vec::new();
    let mut not_measured = 0usize;
    let mut older_version = 0usize;
    let mut refused: HashMap<&'static str, usize> = HashMap::new();
    // The app's own sentence for a frame that has no transparency loss, counted.
    #[cfg_attr(not(feature = "solver"), allow(unused_mut))]
    let mut why: HashMap<String, usize> = HashMap::new();

    for (frame, id) in lights {
        let Some(row) = analyses.get(id) else {
            not_measured += 1;
            continue;
        };
        if row.measure_version != current {
            older_version += 1;
            continue;
        }
        if let Some(verdict) = row.frame_verdict {
            *refused.entry(verdict_label(verdict)).or_default() += 1;
            continue;
        }
        #[cfg(feature = "solver")]
        let value = if metric == QualityMetric::TransparencyLoss {
            let t = transparency.get(id);
            let loss = t.and_then(|t| t.loss_mag).filter(|x| x.is_finite());
            if loss.is_none() {
                if let Some(sentence) = t.and_then(|t| t.reason.clone()) {
                    *why.entry(sentence).or_default() += 1;
                }
            }
            loss
        } else {
            value_of(metric, row)
        };
        #[cfg(not(feature = "solver"))]
        let value = value_of(metric, row);
        match value {
            Some(x) => measured.push((*frame, x)),
            None => not_measured += 1,
        }
    }

    let mut parts: Vec<String> = Vec::new();
    if not_measured > 0 {
        let action = if metric == QualityMetric::TransparencyLoss { SOLVE_THEN_ANALYSE } else { RUN_ANALYSIS };
        let mut part = format!("{not_measured} not measured — {action}");
        if !why.is_empty() {
            let mut reasons: Vec<(String, usize)> = why.into_iter().collect();
            reasons.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let listed: Vec<String> = reasons.iter().map(|(s, n)| format!("{n} {s}")).collect();
            part.push_str(&format!(" ({})", listed.join("; ")));
        }
        parts.push(part);
    }
    if older_version > 0 {
        parts.push(format!("{older_version} measured by an earlier version — run Analysis again"));
    }
    let not_measured_reason = (!parts.is_empty()).then(|| parts.join("; "));

    let mut refused: Vec<(String, usize)> = refused.into_iter().map(|(k, n)| (k.to_string(), n)).collect();
    refused.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut summary = empty(metric, not_measured, not_measured_reason);
    summary.measured = measured.len();
    summary.older_version = older_version;
    summary.refused = refused;
    if measured.is_empty() {
        return Ok(summary);
    }

    let mut values: Vec<f64> = measured.iter().map(|(_, x)| *x).collect();
    values.sort_by(f64::total_cmp);
    let median = median_sorted(&values);
    summary.min = values.first().copied();
    summary.max = values.last().copied();
    summary.median = Some(median);

    let mut deviations: Vec<f64> = values.iter().map(|x| (x - median).abs()).collect();
    deviations.sort_by(f64::total_cmp);
    let mad = median_sorted(&deviations);
    if mad > 0.0 {
        let limit = OUTLIER_SIGMAS * MAD_SIGMA * mad;
        let mut low: Vec<(&ScopedFrame, f64)> = Vec::new();
        let mut high: Vec<(&ScopedFrame, f64)> = Vec::new();
        for (frame, x) in &measured {
            if (x - median).abs() > limit {
                if *x < median { low.push((*frame, *x)) } else { high.push((*frame, *x)) }
            }
        }
        // Worst first: the furthest from the median; ties keep the input order (the sort is stable).
        low.sort_by(|a, b| a.1.total_cmp(&b.1));
        high.sort_by(|a, b| b.1.total_cmp(&a.1));
        let named = |list: Vec<(&ScopedFrame, f64)>| -> Vec<(FileLine, f64)> {
            list.into_iter().take(OUTLIERS_SHOWN).map(|(f, x)| (file_line(f), x)).collect()
        };
        summary.low_outliers = named(low);
        summary.high_outliers = named(high);
    }
    Ok(summary)
}
