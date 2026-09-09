//! Conservative classification from stored FITS cards or XISF FITSKeyword XML.
//! Rules are evidence markers, not proof of full calibration quality. No marker
//! means Unknown; filenames and the acquisition application's name do not prove raw.
use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::models::FileFormat;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Classification {
    pub stage: String,
    pub steps: Vec<String>,
    pub evidence: Vec<String>,
    pub confidence: String,
    pub source_id: Option<String>,
    pub source_name: Option<String>,
}

/// Read processing evidence without touching pixels or inventing source files.
/// `header` is stored FITS text or XISF XML; numeric exposure metadata is handled
/// by the catalog separately. HISTORY substring rules reflect observed Siril
/// output and explicit process records, not filename conventions.
pub fn classify(format: FileFormat, header: &str) -> Classification {
    let keys = parse_stored_header_keys(format, header);
    let mut steps = Vec::new();
    let mut evidence = Vec::new();
    let mut integrated = false;
    for key in ["STACKCNT", "NCOMBINE"] {
        if let Some(v) = keys.get(key).and_then(|v| v.parse::<f64>().ok()) {
            if v > 1.0 {
                integrated = true;
                evidence.push(format!("{key}={v}: multiple input images"));
            }
        }
    }
    let history = history_text(header);
    if history.contains("mean stacking")
        || history.contains("median stacking")
        || history.contains("imageintegration")
    {
        integrated = true;
        evidence.push("Processing history records stacking/integration".into());
    }
    if integrated {
        steps.push("integrated".into());
    }
    if let Some(value) = keys.get("CALSTAT").filter(|v| {
        let value = v.trim().to_ascii_uppercase();
        !value.is_empty() && value.chars().all(|c| "BDFC ".contains(c))
    }) {
        steps.push("calibrated".into());
        evidence.push(format!(
            "CALSTAT={value} (reported processing, not a quality assessment)"
        ));
    } else if history.contains("calibrated with") || history.contains("imagecalibration") {
        steps.push("calibrated".into());
        evidence.push("Processing history records calibration".into());
    }
    if history.contains("debayer") || history.contains("demosaic") {
        steps.push("debayered".into());
        evidence.push("Processing history records debayering/demosaicing".into());
    }
    if history.contains("staralignment")
        || history.contains("registered with")
        || history.contains("registration transformation")
    {
        steps.push("registered".into());
        evidence.push("Processing history records registration".into());
    }
    let stage = if integrated {
        "integrated"
    } else if steps.iter().any(|s| s == "registered") {
        "registered"
    } else if steps.iter().any(|s| s == "debayered") {
        "debayered"
    } else if !steps.is_empty() {
        "calibrated"
    } else {
        "unknown"
    };
    if evidence.is_empty() {
        evidence.push("No supported processing marker; raw status is not inferred".into());
    }
    Classification {
        stage: stage.into(),
        confidence: if steps.is_empty() {
            "unknown"
        } else {
            "header evidence"
        }
        .into(),
        steps,
        evidence,
        source_id: keys.get("ATH_CSRC").filter(|v| !v.is_empty()).cloned(),
        source_name: ["ATH_CSRN", "ORIGNAME", "SRCFILE"]
            .iter()
            .find_map(|k| keys.get(*k).filter(|v| !v.is_empty()).cloned()),
    }
}

/// Restrict process-name matches to HISTORY cards/properties. XISF may put
/// every XML element on one line, so scanning whole lines would mistake an
/// OBJECT name containing a process name for processing evidence.
pub(super) fn history_text(header: &str) -> String {
    if !header.trim_start().starts_with('<') {
        return header
            .lines()
            .filter_map(|l| l.strip_prefix("HISTORY"))
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
    }
    use quick_xml::{events::Event, Reader};
    let mut reader = Reader::from_str(header);
    let mut parts = Vec::new();
    let mut in_property = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let mut name = String::new();
                let mut value = String::new();
                let mut id = String::new();
                for attr in e.attributes() {
                    let attr = match attr {
                        Ok(attr) => attr,
                        Err(error) => {
                            tracing::warn!(%error, "invalid processing history XML attribute");
                            continue;
                        }
                    };
                    match attr.decoded_and_normalized_value(
                        quick_xml::XmlVersion::Implicit1_0,
                        reader.decoder(),
                    ) {
                        Ok(v) => match attr.key.as_ref() {
                            b"name" => name = v.into_owned(),
                            b"value" => value = v.into_owned(),
                            b"id" => id = v.into_owned(),
                            _ => {}
                        },
                        Err(error) => {
                            tracing::warn!(%error, "processing history XML attribute could not be decoded")
                        }
                    }
                }
                if e.local_name().as_ref() == b"FITSKeyword" && name.eq_ignore_ascii_case("HISTORY")
                {
                    parts.push(value.clone());
                }
                if e.local_name().as_ref() == b"Property" {
                    in_property = id
                        .to_ascii_lowercase()
                        .starts_with("pixinsight:processhistory");
                    if in_property {
                        parts.push(value);
                    }
                }
            }
            Ok(Event::Text(t)) if in_property => match t.decode() {
                Ok(v) => parts.push(v.into_owned()),
                Err(error) => {
                    tracing::warn!(%error, "processing history XML text could not be decoded")
                }
            },
            Ok(Event::End(e)) if e.local_name().as_ref() == b"Property" => in_property = false,
            Ok(Event::Eof) => break,
            Err(error) => {
                tracing::warn!(%error,"external processing history XML could not be parsed");
                break;
            }
            _ => {}
        }
    }
    parts.join(" ").to_ascii_lowercase()
}
