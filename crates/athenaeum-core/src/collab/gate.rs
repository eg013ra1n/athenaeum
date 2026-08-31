//! Quality-gate engine (spec §4) — pure, DB-free.
//!
//! Decides whether a light frame is publishable to a collaboration project by
//! layering two checks:
//!
//! - **Layer 1 — hard preconditions** (always on, evaluated in order, all
//!   recorded): the frame must be calibrated, carry an analysis row, have a
//!   known pixel scale, and sit within the project's target radius. A frame can
//!   fail several at once — every reason is collected.
//! - **Layer 2 — threshold rules** (a per-project registry, run only when their
//!   inputs exist): metric-vs-limit comparisons resolved through a small,
//!   extensible metric registry. Unknown metrics/ops are skipped with a
//!   `tracing::warn!`, never fatal.
//!
//! This module holds no DB or HTTP access by design — Task 4 wires it to the
//! catalog. The caller resolves the frame's center, pixel scale, calibration
//! status, and analysis, then hands them here as a [`GateFrameInput`].

use crate::models::FrameAnalysis;

/// A frame's light-calibration status, as layer-1 precondition (1) reads it.
///
/// Lives here (not in `db::`) because the gate is DB-free by design and this is
/// the only surface that still consumes the status: light calibration moved
/// into export and its tracking table is gone (spec 2026-08-31 §8a). The
/// collab-publish rework — the named follow-up that gives this enum a real
/// resolver again — will decide what "calibrated" means for a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightCalStatus {
    NotCalibrated,
    Calibrated,
    Partial,
    Stale,
}

/// The project's target field: a center and an acceptance radius (decimal
/// degrees). A frame whose resolved center lies farther than `radius_deg` from
/// this center is rejected by layer-1 precondition (4).
pub struct ProjectTarget {
    pub ra_deg: f64,
    pub dec_deg: f64,
    pub radius_deg: f64,
}

/// A single project threshold rule as it arrives from the hub / wire.
///
/// BINDING for Task 5 and the wire.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ThresholdRuleView {
    pub metric_key: String,
    pub op: String,
    /// The repo's ts-rs feature set has NO `serde-json-impl`, so
    /// `serde_json::Value` cannot derive `TS` — override the emitted TS type
    /// (the hub validates rule values to number|bool, so this is exact).
    #[ts(type = "number | boolean")]
    pub value: serde_json::Value,
}

/// Everything the gate needs about one frame, resolved by the caller.
pub struct GateFrameInput {
    pub frame_id: i64,
    pub filename: String,
    /// Resolved center, decimal degrees (precedence handled by the caller).
    pub center: Option<(f64, f64)>,
    pub pixel_scale_arcsec: Option<f64>,
    pub cal_status: LightCalStatus,
    pub analysis: Option<FrameAnalysis>,
}

/// The gate's verdict for one frame: echoed metrics, the publishable flag, and
/// every human-readable failure reason.
///
/// BINDING for Task 5 and the wire.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FrameGateRow {
    pub frame_id: i64,
    pub filename: String,
    pub fwhm_arcsec: Option<f64>,
    pub eccentricity: Option<f64>,
    pub stars_detected: Option<i64>,
    pub trailed: Option<bool>,
    pub publishable: bool,
    /// Human-readable failure reasons, empty when publishable
    /// (e.g. `FWHM 3.4″ > 3.0″`, `not calibrated (Stale)`, `no analysis`,
    /// `unknown pixel scale`, `outside target radius (2.1° > 1.5°)`).
    pub failures: Vec<String>,
}

/// Evaluate one frame against the project target and threshold rules (spec §4).
///
/// Layer 1 (preconditions) is always evaluated and every failure is recorded.
/// Layer 2 (rules) runs only for metrics whose inputs exist; a missing input
/// means the layer-1 failure already blocks, so the rule is silently skipped.
pub fn evaluate_frame(
    input: &GateFrameInput,
    target: &ProjectTarget,
    rules: &[ThresholdRuleView],
) -> FrameGateRow {
    let mut failures: Vec<String> = Vec::new();

    if input.cal_status != LightCalStatus::Calibrated {
        failures.push(format!("not calibrated ({:?})", input.cal_status));
    }
    let analysis = input.analysis.as_ref();
    if analysis.is_none() {
        failures.push("no analysis".to_string());
    }
    if input.pixel_scale_arcsec.is_none() {
        failures.push("unknown pixel scale".to_string());
    }
    match input.center {
        Some((ra, dec)) => {
            let d = crate::coordinates::angular_distance(ra, dec, target.ra_deg, target.dec_deg);
            if d > target.radius_deg {
                failures.push(format!(
                    "outside target radius ({d:.1}° > {:.1}°)",
                    target.radius_deg
                ));
            }
        }
        None => failures.push("no coordinates".to_string()),
    }

    // NOTE: FrameAnalysis fields are NOT Option (models.rs) — the Option-ness
    // here comes from "is there an analysis row at all" and "do we know the
    // pixel scale", nothing else.
    let fwhm_arcsec = match (analysis, input.pixel_scale_arcsec) {
        (Some(a), Some(scale)) => Some(a.median_fwhm * scale),
        _ => None,
    };
    let eccentricity = analysis.map(|a| a.median_eccentricity);
    let stars_detected = analysis.map(|a| a.stars_detected);
    let trailed = analysis.map(|a| a.possibly_trailed);

    for rule in rules {
        match rule.metric_key.as_str() {
            "not_trailed" => {
                if rule.op == "reject_if" && rule.value == serde_json::json!(true) {
                    if trailed == Some(true) {
                        failures.push("frame appears trailed".to_string());
                    }
                } else {
                    tracing::warn!(metric_key = %rule.metric_key, op = %rule.op, "unknown gate rule skipped");
                }
            }
            key => {
                let metric: Option<f64> = match key {
                    "fwhm_arcsec" => fwhm_arcsec,
                    "eccentricity" => eccentricity,
                    "stars_detected" => stars_detected.map(|s| s as f64),
                    "median_snr" => analysis.map(|a| a.median_snr),
                    "snr_weight" => analysis.map(|a| a.snr_weight),
                    "frame_snr" => analysis.map(|a| a.frame_snr),
                    _ => {
                        tracing::warn!(metric_key = %rule.metric_key, "unknown gate rule skipped");
                        continue;
                    }
                };
                let Some(limit) = rule.value.as_f64() else {
                    tracing::warn!(metric_key = %rule.metric_key, "non-numeric gate rule value skipped");
                    continue;
                };
                let Some(metric) = metric else { continue }; // layer-1 already recorded the blocker
                let (label, unit) = match key {
                    "fwhm_arcsec" => ("FWHM", "″"),
                    "eccentricity" => ("eccentricity", ""),
                    "stars_detected" => ("stars", ""),
                    other => (other, ""),
                };
                match rule.op.as_str() {
                    "lte" if metric > limit => {
                        failures.push(format!("{label} {metric:.2}{unit} > {limit:.2}{unit}"))
                    }
                    "gte" if metric < limit => {
                        failures.push(format!("{label} {metric:.0} < {limit:.0}"))
                    }
                    "lte" | "gte" => {}
                    other => {
                        tracing::warn!(metric_key = %rule.metric_key, op = %other, "unknown gate op skipped")
                    }
                }
            }
        }
    }

    FrameGateRow {
        frame_id: input.frame_id,
        filename: input.filename.clone(),
        fwhm_arcsec,
        eccentricity,
        stars_detected,
        trailed,
        publishable: failures.is_empty(),
        failures,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::FrameAnalysis;

    /// `FrameAnalysis` fields are NOT `Option` (see `models.rs`): `median_fwhm:
    /// f64`, `stars_detected: i64`, `possibly_trailed: bool`, … Only
    /// `median_beta`/`quality_score`/`config_hash` are optional. Build the full
    /// literal — there is no `Default` impl to lean on.
    fn analysis(fwhm_px: f64, ecc: f64, stars: i64, trailed: bool) -> FrameAnalysis {
        FrameAnalysis {
            id: None,
            frame_id: 1,
            file_id: 1,
            stars_detected: stars,
            median_fwhm: fwhm_px,
            median_eccentricity: ecc,
            median_snr: 10.0,
            median_hfr: 2.0,
            frame_snr: 10.0,
            snr_weight: 1.0,
            psf_signal: 100.0,
            background: 10.0,
            noise: 1.0,
            detection_threshold: 5.0,
            width: 6248,
            height: 4176,
            source_channels: 1,
            trail_r_squared: 0.0,
            possibly_trailed: trailed,
            median_beta: None,
            quality_score: None,
            config_hash: None,
            analyzed_at: "2026-07-13T00:00:00Z".to_string(),
        }
    }

    fn input(analysis_opt: Option<FrameAnalysis>) -> GateFrameInput {
        GateFrameInput {
            frame_id: 1,
            filename: "L_0001.fits".into(),
            center: Some((210.8, 54.35)),
            pixel_scale_arcsec: Some(2.0),
            cal_status: LightCalStatus::Calibrated,
            analysis: analysis_opt,
        }
    }

    fn target() -> ProjectTarget {
        ProjectTarget { ra_deg: 210.8, dec_deg: 54.35, radius_deg: 1.5 }
    }

    fn rules() -> Vec<ThresholdRuleView> {
        serde_json::from_value(serde_json::json!([
            {"metricKey": "fwhm_arcsec", "op": "lte", "value": 3.0},
            {"metricKey": "eccentricity", "op": "lte", "value": 0.6},
            {"metricKey": "stars_detected", "op": "gte", "value": 150},
            {"metricKey": "not_trailed", "op": "reject_if", "value": true}
        ]))
        .unwrap()
    }

    #[test]
    fn passing_frame_is_publishable_with_converted_units() {
        // 1.2 px × 2.0 ″/px = 2.4″ ≤ 3.0″ — the unit conversion is the point.
        let row = evaluate_frame(&input(Some(analysis(1.2, 0.4, 400, false))), &target(), &rules());
        assert!(row.publishable, "failures: {:?}", row.failures);
        assert_eq!(row.fwhm_arcsec, Some(2.4));
        assert_eq!(row.trailed, Some(false));
    }

    #[test]
    fn each_precondition_fails_with_its_reason() {
        // Not calibrated.
        let mut i = input(Some(analysis(1.2, 0.4, 400, false)));
        i.cal_status = LightCalStatus::Stale;
        let row = evaluate_frame(&i, &target(), &rules());
        assert!(!row.publishable);
        assert!(row.failures.iter().any(|f| f.contains("not calibrated")), "{:?}", row.failures);

        // No analysis.
        let row = evaluate_frame(&input(None), &target(), &rules());
        assert!(row.failures.iter().any(|f| f == "no analysis"));

        // Unknown pixel scale.
        let mut i = input(Some(analysis(1.2, 0.4, 400, false)));
        i.pixel_scale_arcsec = None;
        let row = evaluate_frame(&i, &target(), &rules());
        assert!(row.failures.iter().any(|f| f.contains("unknown pixel scale")));

        // Off target (2° away > 1.5° radius) and no coordinates.
        let mut i = input(Some(analysis(1.2, 0.4, 400, false)));
        i.center = Some((210.8, 56.35));
        let row = evaluate_frame(&i, &target(), &rules());
        assert!(row.failures.iter().any(|f| f.contains("outside target radius")));
        let mut i = input(Some(analysis(1.2, 0.4, 400, false)));
        i.center = None;
        let row = evaluate_frame(&i, &target(), &rules());
        assert!(row.failures.iter().any(|f| f == "no coordinates"));
    }

    #[test]
    fn each_rule_fails_with_its_reason() {
        // FWHM: 2.0 px × 2.0 = 4.0″ > 3.0″.
        let row = evaluate_frame(&input(Some(analysis(2.0, 0.4, 400, false))), &target(), &rules());
        assert!(row.failures.iter().any(|f| f.contains("FWHM") && f.contains("3.00")), "{:?}", row.failures);

        // Eccentricity 0.7 > 0.6.
        let row = evaluate_frame(&input(Some(analysis(1.2, 0.7, 400, false))), &target(), &rules());
        assert!(row.failures.iter().any(|f| f.to_lowercase().contains("eccentricity")));

        // Stars 120 < 150.
        let row = evaluate_frame(&input(Some(analysis(1.2, 0.4, 120, false))), &target(), &rules());
        assert!(row.failures.iter().any(|f| f.contains("120") && f.contains("150")));

        // Trailed.
        let row = evaluate_frame(&input(Some(analysis(1.2, 0.4, 400, true))), &target(), &rules());
        assert!(row.failures.iter().any(|f| f.contains("trailed")));
        assert_eq!(row.trailed, Some(true));
    }

    #[test]
    fn unknown_metric_is_skipped_not_fatal() {
        let mut r = rules();
        r.push(
            serde_json::from_value(serde_json::json!(
                {"metricKey": "made_up_metric", "op": "lte", "value": 1.0}
            ))
            .unwrap(),
        );
        let row = evaluate_frame(&input(Some(analysis(1.2, 0.4, 400, false))), &target(), &r);
        assert!(row.publishable, "unknown metric must not block: {:?}", row.failures);
    }

    #[test]
    fn snr_family_rules_apply_generically() {
        let mut a = analysis(1.2, 0.4, 400, false);
        a.median_snr = 4.0;
        let r: Vec<ThresholdRuleView> = serde_json::from_value(serde_json::json!([
            {"metricKey": "median_snr", "op": "gte", "value": 5.0}
        ]))
        .unwrap();
        let row = evaluate_frame(&input(Some(a)), &target(), &r);
        assert!(!row.publishable);
    }
}
