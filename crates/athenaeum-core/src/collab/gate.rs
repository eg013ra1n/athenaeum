//! Quality-gate engine (spec §4) — pure, DB-free.
//!
//! Decides whether a light frame is publishable to a collaboration project by
//! layering two checks:
//!
//! - **Layer 1 — hard preconditions** (always on, evaluated in order, all
//!   recorded): the frame must be calibrated, carry a uuid, use a filter the
//!   project's dictionary recognizes, carry an analysis row, have a known
//!   pixel scale, and sit within the project's target radius. A frame can fail
//!   several at once — every reason is collected.
//! - **Layer 2 — threshold rules** (a per-project registry, run only when their
//!   inputs exist): metric-vs-limit comparisons resolved through a small,
//!   extensible metric registry. Unknown metrics/ops are skipped with a
//!   `tracing::warn!`, never fatal.
//!
//! This module holds no DB or HTTP access by design — `api::collab` wires it
//! to the catalog. The caller resolves the frame's center, pixel scale,
//! calibration verdict, filter match, uuid and analysis, then hands them here
//! as a [`GateFrameInput`].

use crate::models::FrameAnalysis;

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

/// The header pixel-scale fallback used when there is no plate solve:
/// `atan(xpixsz_um / 1000 / focallen_mm)` in arcsec, only when both are
/// present and strictly positive — a `0.0` `xpixsz`/`focallen` is FITS's
/// "not actually set" placeholder (see `plate_solve::hints::is_sentinel_position`'s
/// sibling reasoning for coordinates), not a real value, and must yield
/// `None` ("unknown scale"), never `0.0`. No binning multiply.
///
/// Lives here (pure, DB-free, no feature gate) rather than in either caller
/// so it has exactly one definition: `api::collab::frame_gate_inputs` (the
/// gate's own precedence: plate-solve scale, else this) and
/// `collab::frame_meta::build_frame_meta` (the manifest's `pixelScaleArcsec`/
/// `fwhmArcsec`, same precedence) each read `frames.xpixsz`/`frames.focallen`
/// through their own query and call this with the result — duplicating the
/// formula itself, rather than this function, was fix round 1's finding.
///
/// `#[allow(dead_code)]`: both callers are `render`+`solver`-gated, so under
/// `--no-default-features` (the headless build, where this ungated module
/// still compiles) nothing in the crate calls this — same reasoning as
/// `api::collab::publish_options`'s allow.
#[allow(dead_code)]
pub(crate) fn header_pixel_scale_arcsec(xpixsz: Option<f64>, focallen: Option<f64>) -> Option<f64> {
    match (xpixsz, focallen) {
        (Some(xpixsz), Some(focallen)) if focallen > 0.0 && xpixsz > 0.0 => {
            Some(((xpixsz / 1000.0) / focallen).atan().to_degrees() * 3600.0)
        }
        _ => None,
    }
}

/// The threshold metric registry (collab v3 spec §6.3) — the app's copy. The
/// hub's `src/collab_rules.rs` and the portal's `metrics.ts` are the other two;
/// the test `registry_matches_the_evaluator` pins this one to the match arms
/// below. `lte`/`gte` compare a number; `reject_if` takes the literal `true`.
pub const METRIC_REGISTRY: &[(&str, &[&str])] = &[
    ("fwhm_arcsec", &["lte", "gte"]),
    ("eccentricity", &["lte", "gte"]),
    ("stars_detected", &["lte", "gte"]),
    ("median_snr", &["lte", "gte"]),
    ("snr_weight", &["lte", "gte"]),
    ("frame_snr", &["lte", "gte"]),
    ("not_trailed", &["reject_if"]),
];

/// Everything the gate needs about one frame, resolved by the caller.
pub struct GateFrameInput {
    pub frame_id: i64,
    pub filename: String,
    /// Resolved center, decimal degrees (precedence handled by the caller).
    pub center: Option<(f64, f64)>,
    pub pixel_scale_arcsec: Option<f64>,
    /// The per-frame calibrated verdict (plan ruling P7): `None` when the
    /// frame passes `api::lights::check_mode_ready(.., CalibratedLights)` run
    /// over just this frame; `Some(sentence)` — the exact `Err` text that call
    /// produced, which is also what the Export tab shows under a disabled
    /// Calibrated Lights mode — otherwise. Replaces the old
    /// `LightCalStatus::NotCalibrated` constant (decision C, spec 2026-08-31
    /// §8a).
    pub cal_blocker: Option<String>,
    pub analysis: Option<FrameAnalysis>,
    /// The frame's trimmed `FILTER` header, as read from the catalog.
    pub filter_raw: String,
    /// `filter_raw` resolved against the account's explicit mappings and the
    /// project's cached dictionary (spec 2026-09-28 §3.2,
    /// `collab::filters::FilterResolution`/`resolve_filter`).
    pub filter: crate::collab::filters::FilterResolution,
    /// `frames.uuid` (plan ruling P18 — also `ATH_CSRC`). Empty when the
    /// frame carries none.
    pub uuid: String,
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
    /// Human-readable failure reasons, empty when publishable (e.g.
    /// `FWHM 3.4″ > 3.0″`, the caller's `cal_blocker` sentence verbatim, `frame
    /// has no uuid`, `no FILTER header — needs a filter mapping`,
    /// `filter "OIII" needs a filter mapping`,
    /// `filter "H" is mapped to "Hb", which is not in this project's
    /// dictionary`, `no analysis`, `unknown pixel scale`,
    /// `outside target radius (2.1° > 1.5°)`).
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

    if let Some(blocker) = &input.cal_blocker {
        failures.push(blocker.clone());
    }
    if input.uuid.trim().is_empty() {
        failures.push("frame has no uuid".to_string());
    }
    {
        use crate::collab::filters::FilterResolution;
        match &input.filter {
            FilterResolution::Mapped(_) | FilterResolution::Matched(_) => {}
            FilterResolution::Unmapped if input.filter_raw.trim().is_empty() => {
                failures.push("no FILTER header — needs a filter mapping".to_string());
            }
            FilterResolution::Unmapped => {
                failures.push(format!(
                    "filter \"{}\" needs a filter mapping",
                    input.filter_raw.trim()
                ));
            }
            FilterResolution::MappedToMissing(c) => {
                failures.push(format!(
                    "filter \"{}\" is mapped to \"{c}\", which is not in this project's dictionary",
                    input.filter_raw.trim()
                ));
            }
        }
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

/// Spec 2026-09-28 §7.1: one distinct (camera, raw name) needing a mapping.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UnmappedFilter {
    pub instrume: String,
    pub filter_raw: String,
    pub frames: i64,
}

/// One cause that blocks publishing, with the frames it holds back, the
/// sets they belong to (for the set-scoped actions) and, for `mapFilter`,
/// the raw names (§7.1 table). Kinds in `BLOCKER_ORDER` order.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GateBlocker {
    pub kind: String,
    pub frames: i64,
    pub sets: Vec<i64>,
    pub names: Vec<UnmappedFilter>,
}

pub const BLOCKER_ORDER: [&str; 9] = [
    "analyze",
    "solve",
    "linkCalibration",
    "buildMasters",
    "attest",
    "mapFilter",
    "threshold",
    "uuid",
    "outsideTarget",
];

/// What `derive_blockers` needs per gate row — the caller pairs each
/// `FrameGateRow` with its set and raw filter.
pub struct BlockerRow<'a> {
    pub frame_id: i64,
    pub set_id: Option<i64>,
    pub instrume: &'a str,
    pub filter_raw: &'a str,
    pub filter_unresolved: bool,
    pub failures: &'a [String],
}

/// Fix round 1: `check_mode_ready(.., ExportMode::CalibratedLights)`'s real
/// C-2 sentence is `"{n} master file(s) missing on disk — restore from
/// archive first"` (`api/lights.rs::check_mode_ready`) — the parenthesized
/// `(s)` and the word order mean the old `"master file missing"` substring
/// never matched it, so a frame whose master file was archived or moved fell
/// through to `threshold` with no `attest` entry. `"missing on disk"` matches
/// that sentence (and the sibling `"pre-calibration master file missing on
/// disk"` / raw-originals sentences some day) without over-matching anything
/// else `evaluate_frame`/`frame_cal_verdict` can ever produce.
///
/// `frame_cal_verdict` (`api::collab::frame_cal_verdict`) is the ONE caller
/// that ever produces a `cal_blocker`, and it runs ONLY
/// `check_mode_ready(_, ExportMode::CalibratedLights)` — so every substring
/// here is commented with the exact arm it matches; a `RawWithMasters`/
/// `RawWithCalibrationSets` sentence never reaches this classifier at all.
fn is_calibration_reason(f: &str) -> bool {
    // CalibratedLights: "{n} light(s) have/has no calibration links".
    f.contains("no calibration links")
        // CalibratedLights: "Build masters first — {n} set(s) without a master".
        || f.contains("Build masters first")
        // CalibratedLights, C-2: "{n} master file(s) missing on disk — restore from archive first".
        || f.contains("missing on disk")
        // Not a check_mode_ready arm: frame_cal_verdict's own wrapping when
        // readiness_from_data/collect_export_data itself errored.
        || f.starts_with("could not verify calibration")
        // Not a check_mode_ready arm: frame_gate_inputs's defensive case for a
        // frame with no resolvable frames_set (unreachable in practice).
        || f == "frame set unresolved"
}
/// Same source as [`is_calibration_reason`] — only the two `CalibratedLights`
/// arms that name a missing master. Previously also matched a bare
/// `"no master"`, which was only ever produced by `RawWithMasters`'s
/// `"…have no master — build masters first"` sentence; `frame_cal_verdict`
/// never runs that mode, so the substring never matched anything real and is
/// dropped along with `is_calibration_reason`'s `RawWith*`-only
/// `"No calibration is linked"`.
fn is_build_masters_reason(f: &str) -> bool {
    // CalibratedLights: "Build masters first — {n} set(s) without a master".
    f.contains("Build masters first")
        // CalibratedLights, C-2: "{n} master file(s) missing on disk — restore from archive first".
        || f.contains("missing on disk")
}

/// Spec §7.1 — the table, applied to every row's failure sentences. A row
/// may count under several kinds; each kind counts a frame once.
pub fn derive_blockers(rows: &[BlockerRow<'_>]) -> Vec<GateBlocker> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut frames: BTreeMap<&str, BTreeSet<i64>> = BTreeMap::new();
    let mut sets: BTreeMap<&str, BTreeSet<i64>> = BTreeMap::new();
    let mut names: BTreeMap<(String, String), BTreeSet<i64>> = BTreeMap::new();
    for r in rows {
        let mut add = |kind: &'static str| {
            frames.entry(kind).or_default().insert(r.frame_id);
            if let Some(s) = r.set_id {
                sets.entry(kind).or_default().insert(s);
            }
        };
        for f in r.failures {
            let f = f.as_str();
            if f == "no analysis" {
                add("analyze");
            } else if f == "no coordinates" || f == "unknown pixel scale" {
                add("solve");
            } else if is_build_masters_reason(f) {
                add("buildMasters");
                add("attest");
            } else if is_calibration_reason(f) {
                add("linkCalibration");
                add("attest");
            } else if f.contains("needs a filter mapping") || f.contains("is mapped to") {
                add("mapFilter");
            } else if f == "frame has no uuid" {
                add("uuid");
            } else if f.starts_with("outside target radius") {
                add("outsideTarget");
            } else {
                add("threshold");
            }
        }
        if r.filter_unresolved {
            names
                .entry((
                    r.instrume.trim().to_string(),
                    r.filter_raw.trim().to_string(),
                ))
                .or_default()
                .insert(r.frame_id);
        }
    }
    let mut unmapped: Vec<UnmappedFilter> = names
        .into_iter()
        .map(|((instrume, filter_raw), ids)| UnmappedFilter {
            instrume,
            filter_raw,
            frames: ids.len() as i64,
        })
        .collect();
    unmapped.sort_by(|a, b| {
        b.frames
            .cmp(&a.frames)
            .then(a.instrume.cmp(&b.instrume))
            .then(a.filter_raw.cmp(&b.filter_raw))
    });
    BLOCKER_ORDER
        .iter()
        .filter_map(|kind| {
            let ids = frames.get(kind)?;
            Some(GateBlocker {
                kind: kind.to_string(),
                frames: ids.len() as i64,
                sets: sets
                    .get(kind)
                    .map(|s| s.iter().copied().collect())
                    .unwrap_or_default(),
                names: if *kind == "mapFilter" {
                    unmapped.clone()
                } else {
                    Vec::new()
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::FrameAnalysis;

    /// Fix round 1: the one shared definition, exercised directly (both
    /// callers now just forward their own `xpixsz`/`focallen` read here).
    #[test]
    fn header_pixel_scale_arcsec_matches_the_known_formula_and_treats_zero_as_unset() {
        // 3.76 um / 1000.0 mm focal length ≈ 0.776 ″/px.
        let scale = header_pixel_scale_arcsec(Some(3.76), Some(1000.0)).unwrap();
        assert!((scale - 0.7755556714854275).abs() < 1e-9, "{scale}");

        assert_eq!(header_pixel_scale_arcsec(None, Some(1000.0)), None);
        assert_eq!(header_pixel_scale_arcsec(Some(3.76), None), None);
        assert_eq!(
            header_pixel_scale_arcsec(Some(0.0), Some(1000.0)),
            None,
            "a 0.0 xpixsz is the FITS not-actually-set placeholder"
        );
        assert_eq!(
            header_pixel_scale_arcsec(Some(3.76), Some(0.0)),
            None,
            "a 0.0 focallen is the FITS not-actually-set placeholder"
        );
    }

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
            cal_blocker: None,
            analysis: analysis_opt,
            filter_raw: "L".into(),
            filter: crate::collab::filters::FilterResolution::Matched("L".into()),
            uuid: "11111111-1111-1111-1111-111111111111".into(),
        }
    }

    /// A fully passing `GateFrameInput` — the base every reason-specific test
    /// mutates one field of. See the doc comment above; call sites should not
    /// repeat it.
    fn passing_input() -> GateFrameInput {
        input(Some(analysis(1.2, 0.4, 400, false)))
    }

    fn target() -> ProjectTarget {
        ProjectTarget {
            ra_deg: 210.8,
            dec_deg: 54.35,
            radius_deg: 1.5,
        }
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
        let row = evaluate_frame(
            &input(Some(analysis(1.2, 0.4, 400, false))),
            &target(),
            &rules(),
        );
        assert!(row.publishable, "failures: {:?}", row.failures);
        assert_eq!(row.fwhm_arcsec, Some(2.4));
        assert_eq!(row.trailed, Some(false));
    }

    #[test]
    fn each_precondition_fails_with_its_reason() {
        // Not calibrated.
        let mut i = input(Some(analysis(1.2, 0.4, 400, false)));
        i.cal_blocker = Some("not calibrated".to_string());
        let row = evaluate_frame(&i, &target(), &rules());
        assert!(!row.publishable);
        assert!(
            row.failures.iter().any(|f| f.contains("not calibrated")),
            "{:?}",
            row.failures
        );

        // No analysis.
        let row = evaluate_frame(&input(None), &target(), &rules());
        assert!(row.failures.iter().any(|f| f == "no analysis"));

        // Unknown pixel scale.
        let mut i = input(Some(analysis(1.2, 0.4, 400, false)));
        i.pixel_scale_arcsec = None;
        let row = evaluate_frame(&i, &target(), &rules());
        assert!(row
            .failures
            .iter()
            .any(|f| f.contains("unknown pixel scale")));

        // Off target (2° away > 1.5° radius) and no coordinates.
        let mut i = input(Some(analysis(1.2, 0.4, 400, false)));
        i.center = Some((210.8, 56.35));
        let row = evaluate_frame(&i, &target(), &rules());
        assert!(row
            .failures
            .iter()
            .any(|f| f.contains("outside target radius")));
        let mut i = input(Some(analysis(1.2, 0.4, 400, false)));
        i.center = None;
        let row = evaluate_frame(&i, &target(), &rules());
        assert!(row.failures.iter().any(|f| f == "no coordinates"));
    }

    /// P3 / spec §5.1: the three filter-resolution reasons, each naming the
    /// raw header and (for `MappedToMissing`) the canonical it resolved to —
    /// the operator has to see WHICH filter didn't map and WHY.
    #[test]
    fn filter_reasons_name_the_raw_name_and_the_missing_canonical() {
        use crate::collab::filters::FilterResolution;
        let mut i = passing_input();
        i.filter_raw = String::new();
        i.filter = FilterResolution::Unmapped;
        let row = evaluate_frame(&i, &target(), &[]);
        assert!(
            row.failures
                .iter()
                .any(|f| f == "no FILTER header — needs a filter mapping"),
            "{:?}",
            row.failures
        );

        i.filter_raw = "Slot 0".into();
        let row = evaluate_frame(&i, &target(), &[]);
        assert!(
            row.failures
                .iter()
                .any(|f| f == "filter \"Slot 0\" needs a filter mapping"),
            "{:?}",
            row.failures
        );

        i.filter_raw = "H".into();
        i.filter = FilterResolution::MappedToMissing("Hb".into());
        let row = evaluate_frame(&i, &target(), &[]);
        assert!(
            row.failures.iter().any(|f| f
                == "filter \"H\" is mapped to \"Hb\", which is not in this project's dictionary"),
            "{:?}",
            row.failures
        );

        i.filter = FilterResolution::Matched("Ha".into());
        assert!(evaluate_frame(&i, &target(), &[]).publishable);
    }

    /// Spec §7.1: a row counts under every blocker kind it fails, in
    /// `BLOCKER_ORDER` order; `attest` picks up every calibration reason;
    /// `mapFilter` names the distinct (camera, raw) pairs, most-frames-first.
    #[test]
    fn a_row_counts_under_every_blocker_it_fails() {
        let f1 = vec![
            "3 lights have no calibration links".to_string(),
            "filter \"Slot 0\" needs a filter mapping".to_string(),
        ];
        let f2 = vec![
            "no analysis".to_string(),
            "unknown pixel scale".to_string(),
            "no coordinates".to_string(),
        ];
        let f3 = vec![
            "Build masters first — 1 set without a master".to_string(),
            "FWHM 3.40″ > 3.00″".to_string(),
        ];
        let f4 = vec![
            "frame has no uuid".to_string(),
            "outside target radius (2.1° > 1.5°)".to_string(),
            "no FILTER header — needs a filter mapping".to_string(),
        ];
        let rows = vec![
            BlockerRow {
                frame_id: 1,
                set_id: Some(10),
                instrume: "QHY268M",
                filter_raw: "Slot 0",
                filter_unresolved: true,
                failures: &f1,
            },
            BlockerRow {
                frame_id: 2,
                set_id: Some(10),
                instrume: "QHY268M",
                filter_raw: "L",
                filter_unresolved: false,
                failures: &f2,
            },
            BlockerRow {
                frame_id: 3,
                set_id: Some(11),
                instrume: "ASI294",
                filter_raw: "L",
                filter_unresolved: false,
                failures: &f3,
            },
            BlockerRow {
                frame_id: 4,
                set_id: Some(11),
                instrume: "ATR2600M",
                filter_raw: "",
                filter_unresolved: true,
                failures: &f4,
            },
            BlockerRow {
                frame_id: 5,
                set_id: Some(11),
                instrume: "ATR2600M",
                filter_raw: "",
                filter_unresolved: true,
                failures: &f4,
            },
        ];
        let b = derive_blockers(&rows);
        let kinds: Vec<&str> = b.iter().map(|x| x.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "analyze",
                "solve",
                "linkCalibration",
                "buildMasters",
                "attest",
                "mapFilter",
                "threshold",
                "uuid",
                "outsideTarget"
            ]
        );
        let by = |k: &str| b.iter().find(|x| x.kind == k).unwrap();
        assert_eq!(by("analyze").frames, 1);
        assert_eq!(by("solve").frames, 1, "one frame, two solve reasons");
        assert_eq!(by("linkCalibration").frames, 1);
        assert_eq!(by("linkCalibration").sets, vec![10]);
        assert_eq!(by("buildMasters").frames, 1);
        assert_eq!(by("buildMasters").sets, vec![11]);
        assert_eq!(
            by("attest").frames,
            2,
            "every calibration reason offers attest"
        );
        assert_eq!(by("attest").sets, vec![10, 11]);
        assert_eq!(by("mapFilter").frames, 3);
        assert_eq!(by("mapFilter").names.len(), 2);
        assert_eq!(by("mapFilter").names[0].filter_raw, "", "most frames first");
        assert_eq!(by("mapFilter").names[0].frames, 2);
        assert_eq!(by("mapFilter").names[1].filter_raw, "Slot 0");
        assert_eq!(by("threshold").frames, 1);
        assert_eq!(by("uuid").frames, 2);
        assert_eq!(by("outsideTarget").frames, 2);
        assert!(derive_blockers(&[]).is_empty());
        // A passing row contributes nothing.
        let none: Vec<String> = vec![];
        assert!(derive_blockers(&[BlockerRow {
            frame_id: 9,
            set_id: Some(1),
            instrume: "",
            filter_raw: "L",
            filter_unresolved: false,
            failures: &none
        }])
        .is_empty());
    }

    /// Fix round 1: every failure sentence `check_mode_ready(&_,
    /// ExportMode::CalibratedLights)` can produce — the ONE mode
    /// `frame_cal_verdict` runs (`api::collab::frame_cal_verdict`) — must
    /// classify as a calibration blocker (`buildMasters` or
    /// `linkCalibration`, always with `attest`), never `threshold`. Built
    /// from real `ExportReadiness` values run through the real gate, not
    /// hand-copied strings, so this module's substrings and `api::lights`'
    /// sentences cannot drift apart again — the C-2 mismatch (`"master file
    /// missing"` vs. the real `"master file(s) missing on disk"`) this fix
    /// round found and this test now pins.
    #[test]
    fn every_calibrated_lights_readiness_sentence_is_a_calibration_blocker() {
        use crate::api::lights::{check_mode_ready, ExportReadiness};
        use crate::export::models::ExportMode;

        let ready = ExportReadiness {
            total: 4,
            unlinked_lights: 0,
            raw_sets_without_master: 0,
            raw_set_ids_without_master: vec![],
            missing_master_files: 0,
            missing_raw_calibration_files: 0,
            file_counts: Default::default(),
            raw_sets_buildable: vec![],
            raw_sets_unbuildable: vec![],
            masters_rebuildable: vec![],
            masters_unrebuildable: vec![],
        };
        // The three ways `check_mode_ready` refuses `CalibratedLights`
        // (`api/lights.rs::check_mode_ready_truth_table` exercises the same
        // three against the readiness struct itself), each paired with the
        // ONE blocker kind its sentence must land under — `is_build_masters_reason`
        // runs before `is_calibration_reason` in `derive_blockers`'s `else if`
        // chain, so a masters-shaped sentence never also counts as
        // `linkCalibration` and vice versa.
        let scenarios = [
            (
                ExportReadiness {
                    raw_sets_without_master: 2,
                    raw_set_ids_without_master: vec![7, 9],
                    ..ready.clone()
                },
                "buildMasters",
            ),
            (
                ExportReadiness {
                    unlinked_lights: 3,
                    ..ready.clone()
                },
                "linkCalibration",
            ),
            (
                // C-2: a missing master FILE is a masters problem, not a
                // linking problem — this is the case the C-2 fix was for, so
                // it is pinned to `buildMasters` specifically, not "either".
                ExportReadiness {
                    missing_master_files: 2,
                    ..ready.clone()
                },
                "buildMasters",
            ),
        ];
        for (r, expected_kind) in &scenarios {
            let sentence = check_mode_ready(r, ExportMode::CalibratedLights)
                .expect_err("this scenario must block CalibratedLights");
            let failures = vec![sentence.clone()];
            let rows = [BlockerRow {
                frame_id: 1,
                set_id: Some(10),
                instrume: "",
                filter_raw: "L",
                filter_unresolved: false,
                failures: &failures,
            }];
            let blockers = derive_blockers(&rows);
            let kinds: Vec<&str> = blockers.iter().map(|b| b.kind.as_str()).collect();
            assert!(
                !kinds.contains(&"threshold"),
                "{sentence:?} landed under threshold: {kinds:?}"
            );
            assert!(
                kinds.contains(expected_kind),
                "{sentence:?} must land under {expected_kind}: {kinds:?}"
            );
            assert!(
                kinds.contains(&"attest"),
                "{sentence:?} must also offer attest: {kinds:?}"
            );
        }
    }

    /// P18: a frame with no `frames.uuid` fails the gate — it could never be
    /// announced (`frameUuid` is required) or identified as `ATH_CSRC` on
    /// republish.
    #[test]
    fn empty_uuid_fails() {
        let mut i = input(Some(analysis(1.2, 0.4, 400, false)));
        i.uuid = String::new();
        let row = evaluate_frame(&i, &target(), &rules());
        assert!(!row.publishable);
        assert!(
            row.failures.iter().any(|f| f == "frame has no uuid"),
            "{:?}",
            row.failures
        );
    }

    /// P7: the calibration failure is EXACTLY the caller's `cal_blocker`
    /// sentence — no "not calibrated (...)" wrapping, so it reads identically
    /// to what the Export tab shows for the same frame set under a disabled
    /// Calibrated Lights mode.
    #[test]
    fn cal_blocker_sentence_is_the_failure_text() {
        let mut i = input(Some(analysis(1.2, 0.4, 400, false)));
        i.cal_blocker = Some("2 lights have no calibration links".to_string());
        let row = evaluate_frame(&i, &target(), &rules());
        assert!(!row.publishable);
        assert_eq!(
            row.failures
                .iter()
                .filter(|f| f.as_str() == "2 lights have no calibration links")
                .count(),
            1,
            "{:?}",
            row.failures
        );
    }

    #[test]
    fn each_rule_fails_with_its_reason() {
        // FWHM: 2.0 px × 2.0 = 4.0″ > 3.0″.
        let row = evaluate_frame(
            &input(Some(analysis(2.0, 0.4, 400, false))),
            &target(),
            &rules(),
        );
        assert!(
            row.failures
                .iter()
                .any(|f| f.contains("FWHM") && f.contains("3.00")),
            "{:?}",
            row.failures
        );

        // Eccentricity 0.7 > 0.6.
        let row = evaluate_frame(
            &input(Some(analysis(1.2, 0.7, 400, false))),
            &target(),
            &rules(),
        );
        assert!(row
            .failures
            .iter()
            .any(|f| f.to_lowercase().contains("eccentricity")));

        // Stars 120 < 150.
        let row = evaluate_frame(
            &input(Some(analysis(1.2, 0.4, 120, false))),
            &target(),
            &rules(),
        );
        assert!(row
            .failures
            .iter()
            .any(|f| f.contains("120") && f.contains("150")));

        // Trailed.
        let row = evaluate_frame(
            &input(Some(analysis(1.2, 0.4, 400, true))),
            &target(),
            &rules(),
        );
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
        assert!(
            row.publishable,
            "unknown metric must not block: {:?}",
            row.failures
        );
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

    /// The registry constant and the match arms in `evaluate_frame` are two
    /// statements of the same fact; this test makes them one. Every registry
    /// metric with a satisfiable value must produce a failure when the rule is
    /// violated, and a key outside the registry must be skipped, not merged
    /// into the enforceable set above by mistake.
    #[test]
    fn registry_matches_the_evaluator() {
        assert_eq!(
            METRIC_REGISTRY.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            [
                "fwhm_arcsec",
                "eccentricity",
                "stars_detected",
                "median_snr",
                "snr_weight",
                "frame_snr",
                "not_trailed"
            ]
        );
        let mut a = analysis(1.2, 0.4, 400, true);
        a.median_snr = 1.0;
        a.snr_weight = 1.0;
        a.frame_snr = 1.0;
        for (key, ops) in METRIC_REGISTRY {
            for op in *ops {
                let value = if *key == "not_trailed" {
                    serde_json::json!(true)
                } else if *op == "lte" {
                    serde_json::json!(0.0001)
                } else {
                    serde_json::json!(1_000_000)
                };
                let rule: ThresholdRuleView = serde_json::from_value(
                    serde_json::json!({"metricKey": key, "op": op, "value": value}),
                )
                .unwrap();
                let row = evaluate_frame(&input(Some(a.clone())), &target(), &[rule]);
                assert!(
                    !row.publishable,
                    "{key} {op} must be enforceable, failures: {:?}",
                    row.failures
                );
            }
        }

        // A key outside the registry, appended to an otherwise-passing rule
        // set, must be skipped rather than blocking the frame.
        let mut r = rules();
        r.push(
            serde_json::from_value(serde_json::json!(
                {"metricKey": "made_up", "op": "lte", "value": 1.0}
            ))
            .unwrap(),
        );
        let row = evaluate_frame(&input(Some(analysis(1.2, 0.4, 400, false))), &target(), &r);
        assert!(
            row.publishable,
            "a key outside the registry must not block: {:?}",
            row.failures
        );
    }
}
