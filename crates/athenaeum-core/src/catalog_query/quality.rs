//! How good the frames are: one stored measurement over the raw lights of a scope, as a range, a median and the
//! outliers named by file (MCP plan, Task 3). A value that was not measured is said to be not measured and never
//! estimated; a row written by an earlier measurement version is counted apart and never enters the current figures:
//! each earlier version (and the legacy rows, no version recorded) forms figures of its own (`earlier`).
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

/// What the app says to a user of a frame with no transparency loss. A reason a plate solve or an Analysis cures (not
/// solved, not measured yet, the analysis differs from the solve) reads as that action; any other sentence (no star
/// catalogue, no reference yet, a colour camera, a failed measurement, ...) is the app's own, unchanged.
#[cfg(feature = "solver")]
pub(super) fn transparency_reason_text(sentence: &str) -> String {
    use crate::analysis::transparency::LossReason;
    let cured = [LossReason::NotSolved, LossReason::NotMeasured, LossReason::AnalysisDiffers];
    if cured.iter().any(|r| r.sentence() == sentence) {
        SOLVE_THEN_ANALYSE.to_string()
    } else {
        sentence.to_string()
    }
}

/// The label a failed transparency measurement is counted under in `refused`.
#[cfg(feature = "solver")]
const MEASUREMENT_FAILED: &str = "measurement failed";

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
    /// The rows of earlier measurement versions, one entry per version, newest first, the legacy rows (no version
    /// recorded) last. Their figures are never mixed into the current ones. Empty for `TransparencyLoss`.
    pub earlier: Vec<EarlierFigures>,
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

/// The figures of the rows one earlier measurement version wrote, formed with the current outlier rule over those
/// rows alone.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EarlierFigures {
    /// The measurement version; `None` for a legacy row (written before versions were recorded).
    pub version: Option<u32>,
    /// Rows with a value.
    pub frames: usize,
    pub min: Option<f64>,
    pub median: Option<f64>,
    pub max: Option<f64>,
    pub low_outliers: Vec<(FileLine, f64)>,
    pub high_outliers: Vec<(FileLine, f64)>,
    /// Rows this version refused (a frame-level verdict), by reason, most first.
    pub refused: Vec<(String, usize)>,
}

/// The refusal reason of a legacy row (no version recorded) the app treats as unusable by its trailed flag.
const LEGACY_TRAILED: &str = "stars trailed (legacy flag)";

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
        earlier: Vec::new(),
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
    // The rows of each earlier version, apart: their values and their refusals.
    let mut earlier_rows: HashMap<Option<u32>, (Vec<(&ScopedFrame, f64)>, HashMap<&'static str, usize>)> = HashMap::new();
    // The app's own sentence for a frame whose missing transparency loss no action cures, counted.
    #[cfg_attr(not(feature = "solver"), allow(unused_mut))]
    let mut why: HashMap<String, usize> = HashMap::new();

    for (frame, id) in lights {
        let Some(row) = analyses.get(id) else {
            not_measured += 1;
            continue;
        };
        if row.measure_version != current {
            older_version += 1;
            // The transparency loss is current by its own zero-point version, not by this one.
            if metric != QualityMetric::TransparencyLoss {
                let (values, verdicts) = earlier_rows.entry(row.measure_version).or_default();
                if let Some(verdict) = row.frame_verdict {
                    *verdicts.entry(verdict_label(verdict)).or_default() += 1;
                } else if row.measure_version.is_none() && row.possibly_trailed {
                    // The app judges a legacy row so (`isUnusable`): no version, no verdict, trailed flag set.
                    *verdicts.entry(LEGACY_TRAILED).or_default() += 1;
                } else if let Some(x) = value_of(metric, row) {
                    values.push((*frame, x));
                }
            }
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
                    // A failed measurement is a refusal of the frame, not something an action updates.
                    if sentence == crate::analysis::transparency::LossReason::Refused(
                        crate::analysis::transparency::Refusal::MeasurementFailed,
                    )
                    .sentence()
                    {
                        *refused.entry(MEASUREMENT_FAILED).or_default() += 1;
                        continue;
                    }
                    let text = transparency_reason_text(&sentence);
                    if text != SOLVE_THEN_ANALYSE {
                        *why.entry(text).or_default() += 1;
                    }
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
        if metric == QualityMetric::TransparencyLoss {
            // The action only for the frames it cures; every other frame reads the app's own sentence.
            let own: usize = why.values().sum();
            let mut pieces: Vec<String> = Vec::new();
            if not_measured > own {
                pieces.push(format!("{} not measured — {SOLVE_THEN_ANALYSE}", not_measured - own));
            }
            let mut reasons: Vec<(String, usize)> = why.into_iter().collect();
            reasons.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            pieces.extend(reasons.iter().map(|(s, n)| format!("{n} not measured — {s}")));
            parts.push(pieces.join("; "));
        } else {
            parts.push(format!("{not_measured} not measured — {RUN_ANALYSIS}"));
        }
    }
    let mut earlier: Vec<EarlierFigures> = earlier_rows
        .into_iter()
        .filter(|(_, (values, verdicts))| !values.is_empty() || !verdicts.is_empty())
        .map(|(version, (values, verdicts))| {
            let figures = figures_of(&values);
            EarlierFigures {
                version,
                frames: values.len(),
                min: figures.min,
                median: figures.median,
                max: figures.max,
                low_outliers: figures.low,
                high_outliers: figures.high,
                refused: sorted_refusals(verdicts),
            }
        })
        .collect();
    // Newest version first; `None` (legacy) sorts below every number, so it comes last.
    earlier.sort_by(|a, b| b.version.cmp(&a.version));
    // The earlier rows' own sentence is told with their figures; this one only when they have none to show, and
    // then it is told here alone (the text half prints this reason, nothing else about them).
    if older_version > 0 && earlier.is_empty() {
        parts.push(format!("{older_version} measured by an earlier version — run Analysis again"));
    }
    let not_measured_reason = (!parts.is_empty()).then(|| parts.join("; "));

    let mut summary = empty(metric, not_measured, not_measured_reason);
    summary.measured = measured.len();
    summary.older_version = older_version;
    summary.earlier = earlier;
    summary.refused = sorted_refusals(refused);
    let figures = figures_of(&measured);
    summary.min = figures.min;
    summary.median = figures.median;
    summary.max = figures.max;
    summary.low_outliers = figures.low;
    summary.high_outliers = figures.high;
    Ok(summary)
}

/// Refusal counts as `(reason, count)`, most first, ties by reason.
fn sorted_refusals(refused: HashMap<&'static str, usize>) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = refused.into_iter().map(|(k, n)| (k.to_string(), n)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

/// Range, median and outliers of a set of measured values; all empty for no values.
#[derive(Default)]
struct Figures {
    min: Option<f64>,
    median: Option<f64>,
    max: Option<f64>,
    low: Vec<(FileLine, f64)>,
    high: Vec<(FileLine, f64)>,
}

fn figures_of(measured: &[(&ScopedFrame, f64)]) -> Figures {
    let mut out = Figures::default();
    if measured.is_empty() {
        return out;
    }
    let mut values: Vec<f64> = measured.iter().map(|(_, x)| *x).collect();
    values.sort_by(f64::total_cmp);
    let median = median_sorted(&values);
    out.min = values.first().copied();
    out.max = values.last().copied();
    out.median = Some(median);

    let mut deviations: Vec<f64> = values.iter().map(|x| (x - median).abs()).collect();
    deviations.sort_by(f64::total_cmp);
    let mad = median_sorted(&deviations);
    if mad > 0.0 {
        let limit = OUTLIER_SIGMAS * MAD_SIGMA * mad;
        let mut low: Vec<(&ScopedFrame, f64)> = Vec::new();
        let mut high: Vec<(&ScopedFrame, f64)> = Vec::new();
        for (frame, x) in measured {
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
        out.low = named(low);
        out.high = named(high);
    }
    out
}
