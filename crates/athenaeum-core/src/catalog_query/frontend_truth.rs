//! Facts the frontend computes today, pinned in core so an assistant's answer says what the screens say: the
//! Missing Metadata flags and the Objects page's Stage / WIP / Archive tab. The rows of
//! `tests/fixtures/frontend_truth_tables.json` are what the TypeScript code returns; a Rust test and a vitest test
//! read the same file.

use serde::{Deserialize, Serialize};

use crate::models::Frame;

/// Which of a frame's header facts the Missing Metadata view flags, exactly as the view computes them
/// (`computeMissingFlags`, `MissingMetadataTable.tsx`): the flags read the frame's own values and never its type, so a
/// dark with an empty OBJECT has `object: true`. Which flags apply to which frame type is the caller's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingFlags {
    pub coordinates: bool,
    pub object: bool,
    pub date: bool,
    pub camera: bool,
    pub frame_type: bool,
}

/// A sexagesimal placeholder: only zeros once spaces, signs and colons are stripped (`00 00 00`, `+00:00:00`).
/// Empty text is not a placeholder.
fn string_is_zero(s: Option<&str>) -> bool {
    match s {
        None | Some("") => false,
        Some(s) => s.chars().filter(|c| !c.is_whitespace() && !matches!(c, '+' | '-' | ':')).collect::<String>() == "000000",
    }
}

/// `computeMissingFlags` of the Missing Metadata table.
pub fn missing_flags(frame: &Frame) -> MissingFlags {
    let numeric_missing = (frame.ra.is_none() && frame.dec.is_none()) || (frame.ra == Some(0.0) && frame.dec == Some(0.0));
    let sexagesimal_missing = frame.objctra.is_none()
        || frame.objctdec.is_none()
        || string_is_zero(frame.objctra.as_deref())
        || string_is_zero(frame.objctdec.as_deref());
    MissingFlags {
        coordinates: numeric_missing && sexagesimal_missing,
        object: frame.object.as_deref().is_none_or(str::is_empty),
        date: frame.date_obs.is_none(),
        camera: frame.instrume.as_deref().is_none_or(str::is_empty),
        frame_type: frame.imagetyp.is_none(),
    }
}

/// The Objects page's three tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(crate = "rmcp::schemars")]
pub enum FrameSetTab {
    Stage,
    Wip,
    Archive,
}

/// The tab a frame set is listed under (`Objects.tsx`): archived sets are in Archive whatever else they are, the rest
/// are WIP when custom and Stage when not.
pub fn frame_set_tab(is_custom: bool, is_archived: bool) -> FrameSetTab {
    if is_archived {
        FrameSetTab::Archive
    } else if is_custom {
        FrameSetTab::Wip
    } else {
        FrameSetTab::Stage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Frame, ImageType};
    use chrono::{DateTime, Utc};
    use serde_json::Value;

    fn table() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/frontend_truth_tables.json")).expect("the truth table is JSON")
    }

    fn text(v: &Value) -> Option<String> {
        v.as_str().map(str::to_string)
    }

    fn frame_of(row: &Value) -> Frame {
        let f = &row["frame"];
        Frame {
            ra: f["ra"].as_f64(),
            dec: f["dec"].as_f64(),
            objctra: text(&f["objctra"]),
            objctdec: text(&f["objctdec"]),
            object: text(&f["object"]),
            date_obs: f["date_obs"]
                .as_str()
                .map(|d| DateTime::parse_from_rfc3339(d).expect("an RFC 3339 date").with_timezone(&Utc)),
            instrume: text(&f["instrume"]),
            imagetyp: f["imagetyp"].as_str().map(|t| match t {
                "Light" => ImageType::Light,
                "Dark" => ImageType::Dark,
                "Flat" => ImageType::Flat,
                other => panic!("the table names a type the test does not know: {other}"),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn missing_flags_match_the_frontend_truth_table() {
        let table = table();
        let rows = table["missingFlags"].as_array().expect("missingFlags rows");
        assert!(rows.len() >= 10, "the table is not a stub");
        for row in rows {
            let flags = missing_flags(&frame_of(row));
            let want = &row["flags"];
            let name = row["name"].as_str().unwrap_or("?");
            assert_eq!(flags.coordinates, want["coordinates"].as_bool().unwrap(), "coordinates: {name}");
            assert_eq!(flags.object, want["object"].as_bool().unwrap(), "object: {name}");
            assert_eq!(flags.date, want["date"].as_bool().unwrap(), "date: {name}");
            assert_eq!(flags.camera, want["camera"].as_bool().unwrap(), "camera: {name}");
            assert_eq!(flags.frame_type, want["type"].as_bool().unwrap(), "type: {name}");
        }
        // The row the assistant must not "fix": a dark with an empty OBJECT is flagged, as the view flags it.
        let dark = rows.iter().find(|r| r["frame"]["imagetyp"] == "Dark" && r["frame"]["object"] == "").expect("the dark row");
        assert!(missing_flags(&frame_of(dark)).object);
    }

    #[test]
    fn frame_set_tab_matches_objects_page() {
        let table = table();
        let rows = table["frameSetTab"].as_array().expect("frameSetTab rows");
        assert_eq!(rows.len(), 4);
        for row in rows {
            let tab = frame_set_tab(row["isCustom"].as_bool().unwrap(), row["isArchived"].as_bool().unwrap());
            let said = serde_json::to_value(tab).unwrap();
            assert_eq!(said, row["tab"], "{row}");
        }
    }
}
