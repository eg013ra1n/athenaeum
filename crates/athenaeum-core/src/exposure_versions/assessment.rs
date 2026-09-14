//! Read-only processing assessment. Heuristics are corroborating clues, never
//! proof of an original capture, and do not change exposure accounting or links.
use super::Classification;
use crate::{fits_parser::stored_header::parse_stored_header_keys, models::FileFormat};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingAssessment {
    pub label: String,
    pub candidate_stage: Option<String>,
    pub confidence: String,
    pub evidence: Vec<String>,
}

/// Combine stored header cards, catalog path and last scanned modification time.
/// Times are ISO-8601; timezone-free FITS DATE-OBS is UTC by convention. Missing
/// or malformed evidence remains unresolved. No pixels, files or database rows
/// are modified. A likely-raw result requires all capture clues; none alone is
/// decisive. Path and timestamp heuristics are not calibrated probabilities.
pub fn assess(
    classification: &Classification,
    format: FileFormat,
    header: &str,
    path: &str,
    modified: &str,
) -> ProcessingAssessment {
    let keys = parse_stored_header_keys(format, header);
    let get = |k: &str| keys.get(k).map(String::as_str).unwrap_or("");
    let filename = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let tokens: Vec<String> = path
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let processing_path = tokens.iter().any(|s| {
        matches!(
            s.as_str(),
            "process" | "processed" | "calibrated" | "registered" | "stacked" | "masters" | "pp"
        )
    });
    let capture_app = get("SWCREATE").to_ascii_uppercase().starts_with("N.I.N.A.");
    let sensor =
        !get("INSTRUME").is_empty() && !get("GAIN").is_empty() && !get("XBINNING").is_empty();
    let exposure = get("EXPTIME")
        .parse::<f64>()
        .is_ok_and(|v| v.is_finite() && v >= 0.0);
    let integer = matches!(get("BITPIX"), "8" | "16" | "32" | "64");
    // Match the observed N.I.N.A. local-time filename without inventing a timezone.
    let matching_name = [get("DATE-LOC"), get("DATE-OBS")].iter().any(|date| {
        date.get(..19)
            .is_some_and(|prefix| filename.starts_with(&prefix.replace('T', "_").replace(':', "-")))
    });
    let history = !super::classify::history_text(header).trim().is_empty();
    let processor = [get("SWCREATE"), get("CREATOR")].iter().any(|s| {
        let s = s.to_ascii_lowercase();
        ["siril", "pixinsight", "deepskystacker"]
            .iter()
            .any(|name| s.contains(name))
    });
    let mut evidence = classification.evidence.clone();
    if capture_app {
        evidence.push(
            "Capture software recorded as N.I.N.A.; processed copies may retain this card".into(),
        );
    }
    if sensor && exposure {
        evidence.push("Camera, gain, binning and exposure metadata retained".into());
    }
    if matching_name {
        evidence.push("Filename timestamp matches DATE-LOC or DATE-OBS to the second".into());
    }
    if integer {
        evidence.push(
            "Integer pixel storage is consistent with capture data, but not proof of raw".into(),
        );
    } else if !get("BITPIX").is_empty() {
        evidence.push("Non-integer pixel storage; this alone does not prove calibration".into());
    }
    if processing_path {
        evidence.push(
            "Path/name suggests a processing workspace or derivative; blocks likely-raw suggestion"
                .into(),
        );
    }
    if processor {
        evidence.push("Header names processing software; exact operations may be unknown".into());
    }
    if history && classification.stage == "unknown" {
        evidence.push(
            "Unrecognized processing/history records need review; blocks likely-raw suggestion"
                .into(),
        );
    }
    if !get("DATE").is_empty() {
        evidence.push(format!(
            "Header DATE: {} (file/header creation record, not exposure time)",
            get("DATE")
        ));
    }
    if !modified.is_empty() {
        evidence.push(format!("Last scanned modification time: {modified}; copying or processing may preserve/change it"));
        if let (Some(obs), Some(mtime), Ok(seconds)) = (
            utc(get("DATE-OBS")),
            utc(modified),
            get("EXPTIME").parse::<f64>(),
        ) {
            if seconds.is_finite() && seconds >= 0.0 {
                let delta =
                    mtime.signed_duration_since(obs).num_milliseconds() as f64 / 1000.0 - seconds;
                evidence.push(format!(
                    "Modification time is {delta:.0} s relative to the expected exposure end; timing is contextual evidence only"
                ));
            }
        }
    }
    let (label, candidate, confidence) = if classification.confidence == "user confirmed" {
        (
            format!("{} — user confirmed", classification.stage),
            Some(classification.stage.clone()),
            "user confirmed",
        )
    } else if classification.stage != "unknown" {
        (
            format!("{} — header evidence", classification.stage),
            Some(classification.stage.clone()),
            "header evidence",
        )
    } else if capture_app
        && sensor
        && exposure
        && integer
        && matching_name
        && !processing_path
        && !processor
        && !history
    {
        evidence.push("Combined capture clues suggest uncalibrated data. Confirmation still requires knowing its provenance".into());
        (
            "Likely uncalibrated".into(),
            Some("raw".into()),
            "heuristic, not confirmed",
        )
    } else {
        (
            "Uncertain — review evidence".into(),
            None,
            "insufficient or conflicting evidence",
        )
    };
    ProcessingAssessment {
        label,
        candidate_stage: candidate,
        confidence: confidence.into(),
        evidence,
    }
}

fn utc(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if s.is_empty() {
        return None;
    }
    match chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f").map(|d| d.and_utc())
        }) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::debug!(%error, "processing assessment timestamp unavailable");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Selected cards from the user's N.I.N.A. light; no paths or image pixels.
    const CAPTURE: &str = "BITPIX  = 16\nSWCREATE= 'N.I.N.A. 3.2.0.9001 (x64)'\nINSTRUME= 'QHY268M'\nGAIN    = 56\nXBINNING= 1\nEXPTIME = 300.0\nDATE-LOC= '2026-05-22T05:57:27.1635541'\nDATE-OBS= '2026-05-21T19:57:27.1635541'\n";
    const NAME: &str = "/capture/LIGHT/Eagle/2026-05-22_05-57-27_B_-9.90_300.00s_0001_61.17.fits";
    fn run(header: &str, path: &str) -> ProcessingAssessment {
        assess(
            &super::super::classify(FileFormat::FITS, header),
            FileFormat::FITS,
            header,
            path,
            "2026-05-21T20:02:29+00:00",
        )
    }
    #[test]
    fn observed_capture_combines_clues_without_changing_classification() {
        let classification = super::super::classify(FileFormat::FITS, CAPTURE);
        let a = run(CAPTURE, NAME);
        assert_eq!(classification.stage, "unknown");
        assert_eq!(a.candidate_stage.as_deref(), Some("raw"));
        assert_eq!(a.confidence, "heuristic, not confirmed");
        assert!(a.evidence.iter().any(|s| s.contains("2 s relative")));
    }
    #[test]
    fn copied_dates_or_a_capture_app_alone_do_not_prove_raw() {
        assert_eq!(run("SWCREATE= 'N.I.N.A.'", NAME).candidate_stage, None);
        assert_eq!(
            run(CAPTURE, "/process/light_0001.fit").candidate_stage,
            None
        );
        assert_eq!(
            run(&CAPTURE.replace("BITPIX  = 16", "BITPIX  = -32"), NAME).candidate_stage,
            None
        );
        assert_eq!(
            run(
                &format!("{CAPTURE}HISTORY Unrecognized processing step\n"),
                NAME
            )
            .candidate_stage,
            None
        );
        assert_eq!(
            run("DATE-OBS= '2026-05-21T19:57:27'\n", NAME).candidate_stage,
            None
        );
    }
    #[test]
    fn real_siril_markers_override_inherited_capture_clues() {
        assert_eq!(
            run(
                &format!("{CAPTURE}HISTORY Calibrated with a master dark\nHISTORY Calibrated with a master flat, normalization of 0.480\n"),
                NAME,
            ).candidate_stage.as_deref(),
            Some("calibrated"),
        );
        assert_eq!(
            run(
                &format!("{CAPTURE}STACKCNT= 10\nHISTORY mean stacking with winsorized sigma clipping rejection\n"),
                NAME,
            ).candidate_stage.as_deref(),
            Some("integrated"),
        );
        let mut classification = super::super::classify(FileFormat::FITS, CAPTURE);
        classification.stage = "calibrated".into();
        classification.confidence = "user confirmed".into();
        assert_eq!(
            assess(&classification, FileFormat::FITS, CAPTURE, NAME, "invalid")
                .candidate_stage
                .as_deref(),
            Some("calibrated")
        );
    }
    #[test]
    fn xisf_history_blocks_an_otherwise_capture_like_header() {
        let cards = CAPTURE
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| {
                format!(
                    "<FITSKeyword name=\"{}\" value=\"{}\"/>",
                    key.trim(),
                    value.trim().trim_matches('\'')
                )
            })
            .collect::<String>();
        let xml = format!("<xisf><Image>{cards}</Image></xisf>");
        let classification = super::super::classify(FileFormat::XISF, &xml);
        assert_eq!(
            assess(&classification, FileFormat::XISF, &xml, NAME, "")
                .candidate_stage
                .as_deref(),
            Some("raw")
        );
        let xml = xml.replace(
            "</Image>",
            "<FITSKeyword name=\"HISTORY\" value=\"unrecognized transformation\"/></Image>",
        );
        let classification = super::super::classify(FileFormat::XISF, &xml);
        assert_eq!(
            assess(&classification, FileFormat::XISF, &xml, NAME, "").candidate_stage,
            None
        );
    }
    #[test]
    fn modification_timestamp_does_not_change_the_candidate() {
        let classification = super::super::classify(FileFormat::FITS, CAPTURE);
        for time in [
            "",
            "invalid",
            "2026-09-09T00:00:00Z",
            "2020-01-01T00:00:00Z",
        ] {
            assert_eq!(
                assess(&classification, FileFormat::FITS, CAPTURE, NAME, time)
                    .candidate_stage
                    .as_deref(),
                Some("raw")
            );
        }
    }
}
