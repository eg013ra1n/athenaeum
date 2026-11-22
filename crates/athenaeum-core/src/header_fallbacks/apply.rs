//! The per-field test of Apply and the preview built on it (issue #21).
//!
//! Apply re-reads frames already in the catalog from their stored headers under the current
//! rules. For every frame and every one of the twelve fields it decides whether the column's
//! value came from the header (a value the rules may rewrite) or was set by the user (a value
//! they must keep), and what the header now says. [`plan_frame`] is that test; [`preview`] runs
//! it over the whole catalog without writing anything.
//!
//! A column is "from the header" when it is empty, or equals the value of one of the field's
//! keywords (the ordered ones and the retired ones) in the frame's stored header. A typed value
//! equals none of them and is kept, and `frames.override` plays no part: a plate-solved frame is
//! filled and corrected like any other.

use super::calibration::{movable_sets_of, needs_regroup};
use super::resolve::{resolve_field, resolve_keyword, same_value, FieldValue, ResolvedFields};
use super::{HeaderField, HeaderFallbackConfig};
use crate::fits_parser::header_keys::stored_header_key_map;
use crate::models::FileFormat;
use crate::scanner::{instrume_for_scan, path_has_root_prefix, split_unique_camera_suffix};
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Instant;

/// Frames per page: one read (and, in Apply, one write transaction) of this many frames.
pub const PAGE_SIZE: usize = 500;

// How many frames a field would change in. Plain comments, not docs: ts-rs copies a doc
// comment into the generated declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FieldCount {
    pub field: HeaderField,
    pub frames: usize,
}

// What the rules would change in the catalog. `by_field` holds the non-zero counts only, in the order of `HeaderField::ALL`;
// `frames_without_header` counts the frames that have no usable stored header (never changed).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct HeaderFallbackCounts {
    pub by_field: Vec<FieldCount>,
    pub frames_changed: usize,
    pub values_kept: usize,
    pub sets_regrouped: usize,
    pub frames_without_header: usize,
}

/// One frame as the catalog holds it: the twelve columns and the newest stored header.
#[derive(Debug, Clone)]
pub struct CatalogRow {
    pub frame_id: i64,
    pub file_path: String,
    pub format: String,
    pub imagetyp: Option<String>,
    pub values: ResolvedFields,
    pub header: Option<String>,
}

/// A numeric column read leniently: anything that is not a number is absent.
fn number_at(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Option<f64>> {
    Ok(match row.get_ref(idx)? {
        ValueRef::Integer(i) => Some(i as f64),
        ValueRef::Real(r) => Some(r),
        _ => None,
    })
}

/// A text column read leniently: anything that is not text is absent.
fn text_at(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Option<String>> {
    Ok(match row.get_ref(idx)? {
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        _ => None,
    })
}

/// The next `limit` frames after `after_frame_id`, by frame id. The header is the file's newest
/// stored one (`latest_header_subquery`, the choice the metadata pane and the override recompute
/// make).
pub fn read_rows(conn: &Connection, after_frame_id: i64, limit: usize) -> anyhow::Result<Vec<CatalogRow>> {
    let sql = format!(
        "SELECT fr.id, fi.path, fi.format, fr.imagetyp,
                fr.ccd_temp, fr.set_temp, fr.gain, fr.offset, fr.exptime, fr.focallen,
                fr.xpixsz, fr.ypixsz, fr.object, fr.filter, fr.instrume, fr.telescop,
                {header}
           FROM frames fr
           JOIN files fi ON fi.id = fr.file_id
          WHERE fr.id > ?1
          ORDER BY fr.id
          LIMIT ?2",
        header = crate::db::latest_header_subquery("fr.file_id"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![after_frame_id, limit as i64], |row| {
        Ok(CatalogRow {
            frame_id: row.get(0)?,
            file_path: row.get(1)?,
            format: row.get(2)?,
            imagetyp: text_at(row, 3)?,
            values: ResolvedFields {
                ccd_temp: number_at(row, 4)?,
                set_temp: number_at(row, 5)?,
                gain: number_at(row, 6)?,
                offset: number_at(row, 7)?,
                exptime: number_at(row, 8)?,
                focallen: number_at(row, 9)?,
                xpixsz: number_at(row, 10)?,
                ypixsz: number_at(row, 11)?,
                object: text_at(row, 12)?,
                filter: text_at(row, 13)?,
                instrume: text_at(row, 14)?,
                telescop: text_at(row, 15)?,
            },
            header: text_at(row, 16)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The scan roots, to say which root (and whether it keeps cameras apart) a frame's file is under.
#[derive(Debug, Clone, Default)]
pub struct ScanRoots {
    roots: Vec<(String, i64, bool)>,
}

impl ScanRoots {
    pub fn load(conn: &Connection) -> anyhow::Result<ScanRoots> {
        let mut stmt = conn.prepare("SELECT id, path, unique_camera FROM scan_roots")?;
        let roots = stmt
            .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(0)?, r.get::<_, i64>(2)? != 0)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ScanRoots { roots })
    }

    /// The root `file_path` is under (the longest separator-strict prefix wins): its id and
    /// whether it is a unique-camera root.
    pub fn root_of(&self, file_path: &str) -> Option<(i64, bool)> {
        self.roots
            .iter()
            .filter(|(root, _, _)| path_has_root_prefix(file_path, root))
            .max_by_key(|(root, _, _)| root.trim_end_matches(['/', '\\']).len())
            .map(|(_, id, unique)| (*id, *unique))
    }
}

/// What the test decided for one frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FramePlan {
    /// The columns to write: the field and its new value (`None` clears the column).
    pub changes: Vec<(HeaderField, Option<FieldValue>)>,
    /// Fields whose value was not from the header and so was left alone.
    pub kept: usize,
    /// The frame has no usable stored header: nothing was tested.
    pub no_header: bool,
}

/// A text column's value for the test: trimmed, a blank one is absent (D8).
fn current_value(raw: Option<FieldValue>) -> Option<FieldValue> {
    match raw {
        Some(FieldValue::Text(t)) => {
            let t = t.trim();
            (!t.is_empty()).then(|| FieldValue::Text(t.to_string()))
        }
        other => other,
    }
}

fn text_of(v: &Option<FieldValue>) -> Option<&str> {
    match v {
        Some(FieldValue::Text(t)) => Some(t.as_str()),
        _ => None,
    }
}

/// The `instrume` rule (D9, R4). The scan root's ` N<digits>` suffix is the catalog's own and is
/// never read from, or written by, the header: the base name of the column is compared with the
/// base names of the candidates, and the column's own suffix is kept on the new value. A NULL
/// column takes what a scan of this file would store. `None` = the column is not from the header.
fn instrume_value(
    current: &Option<FieldValue>,
    candidates: &[FieldValue],
    resolved: &Option<FieldValue>,
    root: Option<(i64, bool)>,
) -> Option<Option<FieldValue>> {
    let resolved_text = text_of(resolved);
    let Some(current) = text_of(current) else {
        let (root_id, unique) = root.unwrap_or((0, false));
        return Some(instrume_for_scan(resolved_text.map(str::to_string), unique, root_id).map(FieldValue::Text));
    };
    let (base, suffix) = split_unique_camera_suffix(current);
    let from_header = candidates
        .iter()
        .any(|c| matches!(c, FieldValue::Text(t) if split_unique_camera_suffix(t).0 == base));
    if !from_header {
        return None;
    }
    Some(resolved_text.map(|r| {
        let new_base = split_unique_camera_suffix(r).0;
        FieldValue::Text(match suffix {
            Some(n) => format!("{new_base} N{n}"),
            None => new_base.to_string(),
        })
    }))
}

/// The per-field test of one frame under `rules` (spec §5.4).
pub fn plan_frame(rules: &HeaderFallbackConfig, row: &CatalogRow, roots: &ScanRoots) -> FramePlan {
    let format = match row.format.as_str() {
        "FITS" => FileFormat::FITS,
        "XISF" => FileFormat::XISF,
        _ => return FramePlan { no_header: true, ..FramePlan::default() },
    };
    let Some(text) = row.header.as_deref().filter(|h| !h.trim().is_empty()) else {
        return FramePlan { no_header: true, ..FramePlan::default() };
    };
    let is_xisf = matches!(format, FileFormat::XISF);
    let keys = stored_header_key_map(format, text);
    if !keys.has_keywords() {
        return FramePlan { no_header: true, ..FramePlan::default() };
    }
    let root = roots.root_of(&row.file_path);

    let mut plan = FramePlan::default();
    for field in HeaderField::ALL {
        let raw = row.values.get(field);
        let current = current_value(raw.clone());
        let candidates: Vec<FieldValue> = rules
            .order(field)
            .iter()
            .chain(rules.retired(field))
            .filter_map(|k| resolve_keyword(field, k, &keys))
            .collect();
        // D10: an XISF pixel size the stored dump cannot show (a `<Property>`) is left alone.
        if is_xisf
            && matches!(field, HeaderField::Xpixsz | HeaderField::Ypixsz)
            && candidates.is_empty()
            && current.is_some()
        {
            continue;
        }
        let resolved = resolve_field(field, rules.order(field), &keys);
        let new = if field == HeaderField::Instrume {
            instrume_value(&current, &candidates, &resolved, root)
        } else {
            let from_header = match &current {
                None => true,
                Some(c) => candidates.iter().any(|v| same_value(v, c)),
            };
            from_header.then_some(resolved)
        };
        let Some(new) = new else {
            plan.kept += 1;
            continue;
        };
        let differs = match (&raw, &new) {
            (None, None) => false,
            (Some(a), Some(b)) => !same_value(a, b),
            _ => true,
        };
        if differs {
            plan.changes.push((field, new));
        }
    }
    plan
}

/// Count what the rules would change in the whole catalog. Reads only.
pub fn preview(conn: &Connection, rules: &HeaderFallbackConfig) -> anyhow::Result<HeaderFallbackCounts> {
    let started = Instant::now();
    let roots = ScanRoots::load(conn)?;
    let mut by_field: BTreeMap<HeaderField, usize> = BTreeMap::new();
    let mut counts = HeaderFallbackCounts::default();
    let mut regroup: Vec<i64> = Vec::new();

    let mut after = 0i64;
    loop {
        let rows = read_rows(conn, after, PAGE_SIZE)?;
        let Some(last) = rows.last() else { break };
        after = last.frame_id;
        let page_len = rows.len();
        for row in &rows {
            let plan = plan_frame(rules, row, &roots);
            if plan.no_header {
                counts.frames_without_header += 1;
                continue;
            }
            counts.values_kept += plan.kept;
            if plan.changes.is_empty() {
                continue;
            }
            counts.frames_changed += 1;
            for (field, _) in &plan.changes {
                *by_field.entry(*field).or_insert(0) += 1;
            }
            if needs_regroup(row.imagetyp.as_deref(), &row.values, &plan.changes) {
                regroup.push(row.frame_id);
            }
        }
        if page_len < PAGE_SIZE {
            break;
        }
    }

    counts.sets_regrouped = movable_sets_of(conn, &regroup)?.len();
    counts.by_field = by_field.into_iter().map(|(field, frames)| FieldCount { field, frames }).collect();
    let by_field_text = counts
        .by_field
        .iter()
        .map(|c| format!("{}={}", c.field.id(), c.frames))
        .collect::<Vec<_>>()
        .join(" ");
    tracing::debug!(
        count = counts.frames_changed,
        kept = counts.values_kept,
        no_header = counts.frames_without_header,
        sets_regrouped = counts.sets_regrouped,
        by_field = %by_field_text,
        duration_ms = started.elapsed().as_millis() as u64,
        "header fallbacks previewed"
    );
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::fits_parser::header_keys::XISF_DUMP_BANNER;
    use crate::header_fallbacks::FieldRule;
    use rusqlite::params;

    fn catalog() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    fn add_root(conn: &Connection, id: i64, path: &str, unique_camera: bool) {
        conn.execute(
            "INSERT INTO scan_roots (id, path, unique_camera) VALUES (?1, ?2, ?3)",
            params![id, path, unique_camera as i64],
        )
        .unwrap();
    }

    /// Stored FITS header text: one card per line (the reader pads a short line).
    fn fits_text(cards: &[(&str, &str)]) -> String {
        cards.iter().map(|(k, v)| format!("{k:<8}= {v}")).collect::<Vec<_>>().join("\n")
    }

    fn xisf_text(cards: &[(&str, &str)]) -> String {
        let mut out = XISF_DUMP_BANNER.to_string();
        for (k, v) in cards {
            out.push_str(&format!("{k} = {v}\n"));
        }
        out
    }

    /// Inserts a file, its frame (the twelve columns from `values`) and its header; returns the frame id.
    fn add_frame(
        conn: &Connection,
        path: &str,
        format: &str,
        imagetyp: &str,
        values: &ResolvedFields,
        header: Option<&str>,
    ) -> i64 {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, ?1, 1, '2026-01-01', ?2)",
            params![path, format],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp, ccd_temp, set_temp, gain, offset, exptime, focallen,
                                 xpixsz, ypixsz, object, filter, instrume, telescop)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                file_id,
                imagetyp,
                values.ccd_temp,
                values.set_temp,
                values.gain,
                values.offset,
                values.exptime,
                values.focallen,
                values.xpixsz,
                values.ypixsz,
                values.object,
                values.filter,
                values.instrume,
                values.telescop,
            ],
        )
        .unwrap();
        let frame_id = conn.last_insert_rowid();
        if let Some(h) = header {
            conn.execute("INSERT INTO fits_header (file_id, header) VALUES (?1, ?2)", params![file_id, h]).unwrap();
        }
        frame_id
    }

    fn plan_of(conn: &Connection, rules: &HeaderFallbackConfig, frame_id: i64) -> FramePlan {
        let rows = read_rows(conn, frame_id - 1, 1).unwrap();
        assert_eq!(rows[0].frame_id, frame_id);
        plan_frame(rules, &rows[0], &ScanRoots::load(conn).unwrap())
    }

    fn rules_with(field: HeaderField, order: &[&str], retired: &[&str]) -> HeaderFallbackConfig {
        let mut cfg = HeaderFallbackConfig::default();
        cfg.fields.insert(
            field,
            FieldRule {
                order: order.iter().map(|s| (*s).to_string()).collect(),
                retired: retired.iter().map(|s| (*s).to_string()).collect(),
            },
        );
        cfg
    }

    fn num(v: f64) -> Option<FieldValue> {
        Some(FieldValue::Number(v))
    }

    fn text(v: &str) -> Option<FieldValue> {
        Some(FieldValue::Text(v.to_string()))
    }

    #[test]
    fn needs_regroup_follows_the_effective_temperature() {
        use HeaderField::*;
        let only_set = ResolvedFields { set_temp: Some(-10.0), ..ResolvedFields::default() };
        let both = ResolvedFields { ccd_temp: Some(-10.0), set_temp: Some(-10.0), ..ResolvedFields::default() };
        let set_change = [(SetTemp, num(-20.0))];
        assert!(needs_regroup(Some("Dark"), &only_set, &set_change), "ccd NULL: the set point is the temperature");
        assert!(!needs_regroup(Some("Dark"), &both, &set_change), "ccd present: the set point is not read");
        assert!(needs_regroup(Some("Dark"), &both, &[(CcdTemp, num(-5.0))]));
        // A measured temperature that fills a NULL and equals the set point moves nothing.
        assert!(!needs_regroup(Some("Dark"), &only_set, &[(CcdTemp, num(-10.0))]));
        assert!(needs_regroup(Some("Dark"), &only_set, &[(CcdTemp, num(-9.0))]));
        assert!(needs_regroup(Some("Dark"), &ResolvedFields::default(), &[(CcdTemp, num(-9.0))]));
        // A light has no group; a dark's focal length is not in its key.
        assert!(!needs_regroup(Some("Light"), &both, &[(Gain, num(100.0)), (CcdTemp, num(0.0))]));
        assert!(!needs_regroup(Some("Dark"), &both, &[(Focallen, num(530.0))]));
        assert!(!needs_regroup(None, &both, &[(Gain, num(100.0))]));
        // Key fields per type.
        assert!(needs_regroup(Some("Dark"), &both, &[(Exptime, num(120.0))]));
        assert!(!needs_regroup(Some("Bias"), &both, &[(Exptime, num(120.0))]));
        assert!(needs_regroup(Some("Flat"), &both, &[(Filter, text("Ha"))]));
        assert!(needs_regroup(Some("Flat"), &both, &[(Focallen, num(530.0))]));
    }

    #[test]
    fn null_value_is_filled() {
        let conn = catalog();
        let h = fits_text(&[("DET-TEMP", "-5.5")]);
        let id = add_frame(&conn, "a.fits", "FITS", "Dark", &ResolvedFields::default(), Some(&h));
        let plan = plan_of(&conn, &HeaderFallbackConfig::default(), id);
        assert_eq!(plan.changes, vec![(HeaderField::CcdTemp, num(-5.5))]);
        assert_eq!(plan.kept, 0);
        assert!(!plan.no_header);
    }

    #[test]
    fn header_value_replaced_after_reordering() {
        let conn = catalog();
        let h = fits_text(&[("CCD-TEMP", "-10"), ("DET-TEMP", "12")]);
        let values = ResolvedFields { ccd_temp: Some(-10.0), ..ResolvedFields::default() };
        let id = add_frame(&conn, "a.fits", "FITS", "Light", &values, Some(&h));
        let rules = rules_with(HeaderField::CcdTemp, &["DET-TEMP", "CCD-TEMP"], &[]);
        let plan = plan_of(&conn, &rules, id);
        assert_eq!(plan.changes, vec![(HeaderField::CcdTemp, num(12.0))]);
        // Under the default order the column is already what the header says.
        assert!(plan_of(&conn, &HeaderFallbackConfig::default(), id).changes.is_empty());
    }

    #[test]
    fn typed_value_is_kept() {
        let conn = catalog();
        let h = fits_text(&[("CCD-TEMP", "-10")]);
        let values = ResolvedFields { ccd_temp: Some(-20.0), ..ResolvedFields::default() };
        let id = add_frame(&conn, "a.fits", "FITS", "Light", &values, Some(&h));
        let plan = plan_of(&conn, &HeaderFallbackConfig::default(), id);
        assert!(plan.changes.is_empty());
        assert_eq!(plan.kept, 1);
    }

    #[test]
    fn plate_solved_frame_is_filled() {
        let conn = catalog();
        let h = fits_text(&[("FOCALLEN", "530"), ("DET-TEMP", "-7")]);
        let values = ResolvedFields { focallen: Some(812.0), ..ResolvedFields::default() };
        let id = add_frame(&conn, "a.fits", "FITS", "Light", &values, Some(&h));
        conn.execute("UPDATE frames SET override = 1 WHERE id = ?1", params![id]).unwrap();
        let plan = plan_of(&conn, &HeaderFallbackConfig::default(), id);
        assert_eq!(plan.changes, vec![(HeaderField::CcdTemp, num(-7.0))]);
        assert_eq!(plan.kept, 1, "the solved focal length is the user's");
    }

    #[test]
    fn retired_keyword_value_is_cleared() {
        let conn = catalog();
        let h = fits_text(&[("MY-TEMP", "-3")]);
        let values = ResolvedFields { ccd_temp: Some(-3.0), ..ResolvedFields::default() };
        let id = add_frame(&conn, "a.fits", "FITS", "Light", &values, Some(&h));
        let rules = rules_with(HeaderField::CcdTemp, &["CCD-TEMP", "DET-TEMP"], &["MY-TEMP"]);
        let plan = plan_of(&conn, &rules, id);
        assert_eq!(plan.changes, vec![(HeaderField::CcdTemp, None)]);
        // Without the retirement the value is nobody's and stays.
        let plan = plan_of(&conn, &HeaderFallbackConfig::default(), id);
        assert!(plan.changes.is_empty());
        assert_eq!(plan.kept, 1);
    }

    #[test]
    fn unique_camera_suffix_is_kept() {
        let conn = catalog();
        add_root(&conn, 7, "/cams/seven", true);
        add_root(&conn, 8, "/cams/eight", false);
        let h = fits_text(&[("INSTRUME", "'Cam'"), ("CAMERA", "'Cam2'")]);
        let rules = rules_with(HeaderField::Instrume, &["CAMERA", "INSTRUME"], &[]);
        let suffixed = ResolvedFields { instrume: Some("Cam N7".into()), ..ResolvedFields::default() };
        let id = add_frame(&conn, "/cams/seven/a.fits", "FITS", "Light", &suffixed, Some(&h));
        assert_eq!(plan_of(&conn, &rules, id).changes, vec![(HeaderField::Instrume, text("Cam2 N7"))]);

        let empty = ResolvedFields::default();
        let unique = add_frame(&conn, "/cams/seven/b.fits", "FITS", "Light", &empty, Some(&h));
        assert_eq!(plan_of(&conn, &rules, unique).changes, vec![(HeaderField::Instrume, text("Cam2 N7"))]);
        let plain = add_frame(&conn, "/cams/eight/c.fits", "FITS", "Light", &empty, Some(&h));
        assert_eq!(plan_of(&conn, &rules, plain).changes, vec![(HeaderField::Instrume, text("Cam2"))]);
        let no_root = add_frame(&conn, "/elsewhere/d.fits", "FITS", "Light", &empty, Some(&h));
        assert_eq!(plan_of(&conn, &rules, no_root).changes, vec![(HeaderField::Instrume, text("Cam2"))]);

        // A typed camera name is the user's.
        let typed = ResolvedFields { instrume: Some("My Camera N7".into()), ..ResolvedFields::default() };
        let kept = add_frame(&conn, "/cams/seven/e.fits", "FITS", "Light", &typed, Some(&h));
        let plan = plan_of(&conn, &rules, kept);
        assert!(plan.changes.is_empty());
        assert_eq!(plan.kept, 1);
    }

    #[test]
    fn a_suffixed_header_in_a_unique_root_previews_nothing() {
        // R4: the scan stores `Cam N3` read in unique root 5 as `Cam N5`; Apply compares base with base.
        let conn = catalog();
        add_root(&conn, 5, "/cams/five", true);
        let h = fits_text(&[("INSTRUME", "'Cam N3'")]);
        let values = ResolvedFields { instrume: Some("Cam N5".into()), ..ResolvedFields::default() };
        let id = add_frame(&conn, "/cams/five/a.fits", "FITS", "Light", &values, Some(&h));
        let plan = plan_of(&conn, &HeaderFallbackConfig::default(), id);
        assert!(plan.changes.is_empty(), "{:?}", plan.changes);
        assert_eq!(plan.kept, 0);
        assert_eq!(preview(&conn, &HeaderFallbackConfig::default()).unwrap(), HeaderFallbackCounts::default());
    }

    #[test]
    fn padded_xisf_text_is_rewritten_trimmed() {
        let conn = catalog();
        let h = xisf_text(&[("FILTER", "Ha")]);
        let values = ResolvedFields { filter: Some("Ha  ".into()), ..ResolvedFields::default() };
        let id = add_frame(&conn, "a.xisf", "XISF", "Light", &values, Some(&h));
        let plan = plan_of(&conn, &HeaderFallbackConfig::default(), id);
        assert_eq!(plan.changes, vec![(HeaderField::Filter, text("Ha"))]);
        assert_eq!(plan.kept, 0);
    }

    #[test]
    fn xisf_property_pixel_size_is_left_and_not_counted() {
        let conn = catalog();
        let h = xisf_text(&[("FILTER", "Ha")]);
        let values = ResolvedFields {
            xpixsz: Some(3.76),
            ypixsz: Some(3.76),
            filter: Some("Ha".into()),
            ..ResolvedFields::default()
        };
        let id = add_frame(&conn, "a.xisf", "XISF", "Light", &values, Some(&h));
        let plan = plan_of(&conn, &HeaderFallbackConfig::default(), id);
        assert!(plan.changes.is_empty());
        assert_eq!(plan.kept, 0);
    }

    #[test]
    fn placeholder_header_is_counted_apart() {
        let conn = catalog();
        let values = ResolvedFields { ccd_temp: Some(-5.0), filter: Some("Ha".into()), ..ResolvedFields::default() };
        let placeholder = "Header extracted from: b.xisf\n(No FITS keywords found in XISF)";
        add_frame(&conn, "b.xisf", "XISF", "Light", &values, Some(placeholder));
        add_frame(&conn, "c.fits", "FITS", "Light", &values, None);
        let counts = preview(&conn, &HeaderFallbackConfig::default()).unwrap();
        assert_eq!(counts.frames_without_header, 2);
        assert_eq!(counts, HeaderFallbackCounts { frames_without_header: 2, ..HeaderFallbackCounts::default() });
        assert!(plan_of(&conn, &HeaderFallbackConfig::default(), 1).no_header);
    }

    #[test]
    fn the_newest_stored_header_is_the_one_tested() {
        let conn = catalog();
        let old = fits_text(&[("DET-TEMP", "-1")]);
        let id = add_frame(&conn, "a.fits", "FITS", "Light", &ResolvedFields::default(), Some(&old));
        let file_id: i64 = conn.query_row("SELECT file_id FROM frames WHERE id = ?1", params![id], |r| r.get(0)).unwrap();
        let new = fits_text(&[("DET-TEMP", "-2")]);
        conn.execute("INSERT INTO fits_header (file_id, header) VALUES (?1, ?2)", params![file_id, new]).unwrap();
        assert_eq!(
            plan_of(&conn, &HeaderFallbackConfig::default(), id).changes,
            vec![(HeaderField::CcdTemp, num(-2.0))]
        );
    }

    fn calibration_set(conn: &Connection, imagetyp: &str) -> i64 {
        conn.execute("INSERT INTO calibration_set (imagetyp, date) VALUES (?1, '2026-01-01')", params![imagetyp]).unwrap();
        conn.last_insert_rowid()
    }

    fn put_in_set(conn: &Connection, set_id: i64, frame_id: i64) {
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            params![set_id, frame_id],
        )
        .unwrap();
    }

    #[test]
    fn preview_counts_fields_frames_kept_and_sets() {
        let conn = catalog();
        let h = fits_text(&[("DET-TEMP", "-5.5")]);
        let none = ResolvedFields::default();
        let raw_set = calibration_set(&conn, "Dark");
        for i in 0..3 {
            let id = add_frame(&conn, &format!("d{i}.fits"), "FITS", "Dark", &none, Some(&h));
            put_in_set(&conn, raw_set, id);
        }
        // A light whose gain the user typed over the header's.
        let typed = ResolvedFields { gain: Some(100.0), ..ResolvedFields::default() };
        add_frame(&conn, "l.fits", "FITS", "Light", &typed, Some(&fits_text(&[("GAIN", "200")])));
        // A dark of a set a master replaced: it changes, its set is not counted.
        let master = calibration_set(&conn, "MasterDark");
        let frozen = calibration_set(&conn, "Dark");
        conn.execute("UPDATE calibration_set SET superseded_by_set_id = ?1 WHERE id = ?2", params![master, frozen]).unwrap();
        let id = add_frame(&conn, "s.fits", "FITS", "Dark", &none, Some(&h));
        put_in_set(&conn, frozen, id);

        let counts = preview(&conn, &HeaderFallbackConfig::default()).unwrap();
        assert_eq!(
            counts,
            HeaderFallbackCounts {
                by_field: vec![FieldCount { field: HeaderField::CcdTemp, frames: 4 }],
                frames_changed: 4,
                values_kept: 1,
                sets_regrouped: 1,
                frames_without_header: 0,
            }
        );
    }

    #[test]
    fn preview_pages_through_more_frames_than_one_page() {
        let conn = catalog();
        let h = fits_text(&[("DET-TEMP", "-5.5")]);
        let n = PAGE_SIZE + 7;
        conn.execute_batch("BEGIN").unwrap();
        for i in 0..n {
            add_frame(&conn, &format!("p{i}.fits"), "FITS", "Light", &ResolvedFields::default(), Some(&h));
        }
        conn.execute_batch("COMMIT").unwrap();
        let counts = preview(&conn, &HeaderFallbackConfig::default()).unwrap();
        assert_eq!(counts.frames_changed, n);
        assert_eq!(counts.by_field, vec![FieldCount { field: HeaderField::CcdTemp, frames: n }]);
    }

    // ── a catalog this build just scanned previews nothing (anti-goal, spec §2) ──

    /// A FITS file made of `cards` (a card is at most 80 characters, padded here).
    fn fits_file(cards: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for c in ["SIMPLE  =                    T", "BITPIX  =                    8", "NAXIS   =                    0"]
            .iter()
            .copied()
            .chain(cards.iter().copied())
            .chain(std::iter::once("END"))
        {
            assert!(c.len() <= 80, "card too long: {c:?}");
            out.extend(format!("{c:<80}").into_bytes());
        }
        while out.len() % 2880 != 0 {
            out.push(b' ');
        }
        out
    }

    fn scan_root_with(root: &std::path::Path, unique_camera: bool) -> Connection {
        let conn = catalog();
        add_root(&conn, 1, root.to_str().unwrap(), unique_camera);
        let result = crate::scanner::scan_directory(root, &conn, None, unique_camera, 1);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        conn
    }

    #[test]
    fn default_rules_on_a_freshly_scanned_catalog_preview_zero() {
        use crate::fits_writer::{write_xisf_f32, Card, CardValue};
        let dir = tempfile::tempdir().unwrap();
        // FITS: DET-TEMP only, a HIERARCH default keyword, a CONTINUE string, a doubled quote, D notation.
        std::fs::write(
            dir.path().join("a.fits"),
            fits_file(&[
                "IMAGETYP= 'Light   '",
                "DET-TEMP=                -5.5",
                "HIERARCH GAIN = 120",
                "EXPTIME =              1.2D+02",
                "OBJECT  = 'NGC 7000 North &'",
                "CONTINUE  'America'",
                "FILTER  = 'O''III   '",
                "INSTRUME= 'Cam     '",
                "TELESCOP= 'Scope   '",
                "PIXSIZE1=                3.76",
                "PIXSIZE2=                3.76",
            ]),
        )
        .unwrap();
        // XISF: EXPOSURE only, a padded FILTER, a blank OBJECT.
        let cards = vec![
            Card::new("IMAGETYP", CardValue::Str("Light".into())).unwrap(),
            Card::new("EXPOSURE", CardValue::Real(120.0)).unwrap(),
            Card::new("FILTER", CardValue::Str("Ha  ".into())).unwrap(),
            Card::new("OBJECT", CardValue::Str("".into())).unwrap(),
        ];
        write_xisf_f32(&dir.path().join("b.xisf"), 2, 2, 1, &[0.0, 0.25, 0.5, 1.0], &cards).unwrap();

        let conn = scan_root_with(dir.path(), false);
        let frames: i64 = conn.query_row("SELECT COUNT(*) FROM frames", [], |r| r.get(0)).unwrap();
        assert_eq!(frames, 2);
        assert_eq!(preview(&conn, &HeaderFallbackConfig::default()).unwrap(), HeaderFallbackCounts::default());
    }

    #[test]
    fn a_scanned_unique_camera_catalog_previews_zero_even_for_a_suffixed_header() {
        let dir = tempfile::tempdir().unwrap();
        for (name, instrume, seq) in [("a.fits", "Cam", 1), ("b.fits", "Cam N3", 2)] {
            let instrume_card = format!("INSTRUME= '{instrume}'");
            let seq_card = format!("SEQNUM  = {seq:>20}");
            std::fs::write(
                dir.path().join(name),
                fits_file(&["IMAGETYP= 'Light   '", instrume_card.as_str(), seq_card.as_str()]),
            )
            .unwrap();
        }
        let conn = scan_root_with(dir.path(), true);
        let stored: Vec<String> = conn
            .prepare("SELECT instrume FROM frames ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(stored, ["Cam N1", "Cam N1"]);
        assert_eq!(preview(&conn, &HeaderFallbackConfig::default()).unwrap(), HeaderFallbackCounts::default());
    }
}
