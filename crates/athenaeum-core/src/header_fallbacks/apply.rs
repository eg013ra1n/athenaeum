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

// How many frames a field would change in. Plain comments here by choice: ts-rs copies only
// field-level docs into the declaration, so a struct-level `///` would also be safe.
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

/// A numeric column read leniently: anything that is not a finite number is absent.
fn number_at(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Option<f64>> {
    Ok(match row.get_ref(idx)? {
        ValueRef::Integer(i) => Some(i as f64),
        // A stored +-inf or NaN is no value (D7): the resolver never produces one, a scan stores NULL.
        ValueRef::Real(r) if r.is_finite() => Some(r),
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

/// The `instrume` rule (D9, R4, R5). The scan root's ` N<digits>` suffix belongs to the catalog,
/// and a row's INSTRUME can be in any of the forms the catalog's writers give it, so the column is
/// matched against each candidate in the forms a scan, a master registration and a move produce.
/// The first match wins; `None` = the column is not from the header.
///
/// 1. NULL column: what a scan of this file would store.
/// 2. The column is the scan form of a candidate (the scan and re-parse form; in a plain root
///    the candidate itself): the scan form of the new value.
/// 3. The column is a candidate exactly (a master registered without the suffix): the new value
///    unchanged.
/// 4. The column has the base name of a candidate (a moved, adopted or reconciled row): the base
///    of the new value, with the column's own suffix if it has one.
fn instrume_value(
    current: &Option<FieldValue>,
    candidates: &[FieldValue],
    resolved: &Option<FieldValue>,
    root: Option<(i64, bool)>,
) -> Option<Option<FieldValue>> {
    let (root_id, unique) = root.unwrap_or((0, false));
    let resolved_text = text_of(resolved);
    let scan_form = |t: Option<&str>| instrume_for_scan(t.map(str::to_string), unique, root_id).map(FieldValue::Text);
    let Some(current) = text_of(current) else {
        return Some(scan_form(resolved_text));
    };
    let names: Vec<&str> = candidates
        .iter()
        .filter_map(|c| match c {
            FieldValue::Text(t) => Some(t.as_str()),
            FieldValue::Number(_) => None,
        })
        .collect();
    if names
        .iter()
        .any(|c| instrume_for_scan(Some((*c).to_string()), unique, root_id).as_deref() == Some(current))
    {
        return Some(scan_form(resolved_text));
    }
    if names.contains(&current) {
        return Some(resolved.clone());
    }
    let (base, suffix) = split_unique_camera_suffix(current);
    if names.iter().any(|c| split_unique_camera_suffix(c).0 == base) {
        return Some(resolved_text.map(|r| {
            let new_base = split_unique_camera_suffix(r).0;
            FieldValue::Text(match suffix {
                Some(n) => format!("{new_base} N{n}"),
                None => new_base.to_string(),
            })
        }));
    }
    None
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
        let resolved = resolve_field(field, rules.order(field), &keys);
        // D10: an XISF pixel size the stored dump cannot show (a `<Property>`) is left alone: the
        // order chain resolves nothing, so the scan took the Property and Apply cannot tell.
        if is_xisf && matches!(field, HeaderField::Xpixsz | HeaderField::Ypixsz) && resolved.is_none() && current.is_some()
        {
            continue;
        }
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
        scan_root_under(root, unique_camera, &HeaderFallbackConfig::default())
    }

    /// A catalog scanned from `root` under `rules` (stored first: the scanner loads them itself).
    fn scan_root_under(root: &std::path::Path, unique_camera: bool, rules: &HeaderFallbackConfig) -> Connection {
        let conn = catalog();
        crate::header_fallbacks::store(&conn, rules).unwrap();
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
        // XISF: EXPTIME in Fortran D notation (a string value, read as a number).
        let cards = vec![
            Card::new("IMAGETYP", CardValue::Str("Light".into())).unwrap(),
            Card::new("EXPTIME", CardValue::Str("1.2D+02".into())).unwrap(),
        ];
        write_xisf_f32(&dir.path().join("c.xisf"), 2, 2, 1, &[0.0, 0.25, 0.5, 1.0], &cards).unwrap();

        let conn = scan_root_with(dir.path(), false);
        let stored = stored_by_name(&conn);
        assert_eq!(stored.len(), 3);
        let a = &stored["a.fits"];
        assert_eq!(a.ccd_temp, Some(-5.5));
        assert_eq!(a.gain, Some(120.0));
        assert_eq!(a.exptime, Some(120.0));
        // The CONTINUE card joins the two pieces (the reader drops the space before the `&`).
        let object = a.object.as_deref().unwrap();
        assert!(object.starts_with("NGC 7000 North") && object.ends_with("America"), "{object:?}");
        assert_eq!(a.filter.as_deref(), Some("O'III"));
        assert_eq!(stored["b.xisf"].exptime, Some(120.0));
        assert_eq!(stored["b.xisf"].filter.as_deref(), Some("Ha"));
        assert_eq!(stored["c.xisf"].exptime, Some(120.0));
        assert_eq!(preview(&conn, &HeaderFallbackConfig::default()).unwrap(), HeaderFallbackCounts::default());
    }

    /// The twelve columns of every frame, by file name.
    fn stored_by_name(conn: &Connection) -> BTreeMap<String, ResolvedFields> {
        read_rows(conn, 0, 100_000)
            .unwrap()
            .into_iter()
            .map(|r| {
                let name = std::path::Path::new(&r.file_path).file_name().unwrap().to_string_lossy().into_owned();
                (name, r.values)
            })
            .collect()
    }

    /// What Apply will write for each planned change, standing in for it in these tests.
    fn write_changes(conn: &Connection, frame_id: i64, changes: &[(HeaderField, Option<FieldValue>)]) {
        use rusqlite::types::Value;
        for (field, value) in changes {
            let v = match value {
                Some(FieldValue::Number(n)) => Value::Real(*n),
                Some(FieldValue::Text(t)) => Value::Text(t.clone()),
                None => Value::Null,
            };
            conn.execute(&format!("UPDATE frames SET {} = ?1 WHERE id = ?2", field.column()), params![v, frame_id])
                .unwrap();
        }
    }

    /// A card line: keyword, then the value part as the stored header text would carry it.
    fn card_line(key: &str, value: &str) -> String {
        format!("{key:<8}= {value}")
    }

    fn real(v: &str) -> String {
        format!("{v:>20}")
    }

    fn quoted(v: &str) -> String {
        format!("'{v}'")
    }

    fn write_fits(dir: &std::path::Path, name: &str, cards: &[(&str, String)]) {
        let lines: Vec<String> = cards.iter().map(|(k, v)| card_line(k, v)).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        std::fs::write(dir.join(name), fits_file(&refs)).unwrap();
    }

    /// Files that tell the default rules and the custom ones apart in every field they touch,
    /// the camera name in the forms of the review's two traced cases among them.
    fn rules_fixture(dir: &std::path::Path) {
        use crate::fits_writer::{write_xisf_f32, Card, CardValue};
        write_fits(
            dir,
            "a.fits",
            &[
                ("IMAGETYP", quoted("Light")),
                ("DET-TEMP", real("-5.5")),
                ("CCD-TEMP", real("-10")),
                ("MY-TEMP", real("3")),
                ("EXPOSURE", real("60")),
                ("EXPTIME", real("120")),
                ("MYGAIN", real("77")),
                ("GAIN", real("120")),
                ("OFFSET", real("10")),
                ("CAMERA", quoted("Other")),
                ("INSTRUME", quoted("Cam")),
                ("FILTER", quoted("Ha")),
                ("OLDFILT", quoted("Lum")),
                ("OBJECT", quoted("M31")),
                ("TELESCOP", quoted("Scope")),
                ("FOCALLEN", real("530")),
                ("XPIXSZ", real("4.5")),
                ("PIXSIZE1", real("3.76")),
            ],
        );
        // Case (a): the header's own INSTRUME ends in ` N3`, and CAMERA names another camera.
        write_fits(
            dir,
            "b.fits",
            &[
                ("IMAGETYP", quoted("Dark")),
                ("DET-TEMP", real("-10")),
                ("EXPTIME", real("300")),
                ("INSTRUME", quoted("Cam N3")),
                ("CAMERA", quoted("Other")),
            ],
        );
        // Case (b): CAMERA carries the ` N3`, INSTRUME does not.
        write_fits(
            dir,
            "c.fits",
            &[("IMAGETYP", quoted("Light")), ("INSTRUME", quoted("Cam")), ("CAMERA", quoted("Cam N3"))],
        );
        write_fits(dir, "d.fits", &[("IMAGETYP", quoted("Light")), ("INSTRUME", quoted("Cam"))]);
        let cards = vec![
            Card::new("IMAGETYP", CardValue::Str("Light".into())).unwrap(),
            Card::new("EXPOSURE", CardValue::Real(60.0)).unwrap(),
            Card::new("EXPTIME", CardValue::Str("1.2D+02".into())).unwrap(),
            Card::new("INSTRUME", CardValue::Str("XCam N2".into())).unwrap(),
            Card::new("CAMERA", CardValue::Str("XOther".into())).unwrap(),
            Card::new("FILTER", CardValue::Str("Ha  ".into())).unwrap(),
            Card::new("MYGAIN", CardValue::Real(77.0)).unwrap(),
            Card::new("GAIN", CardValue::Real(120.0)).unwrap(),
        ];
        write_xisf_f32(&dir.join("e.xisf"), 2, 2, 1, &[0.0, 0.25, 0.5, 1.0], &cards).unwrap();
    }

    fn set_rule(cfg: &mut HeaderFallbackConfig, field: HeaderField, order: &[&str], retired: &[&str]) {
        cfg.fields.insert(
            field,
            FieldRule {
                order: order.iter().map(|s| (*s).to_string()).collect(),
                retired: retired.iter().map(|s| (*s).to_string()).collect(),
            },
        );
    }

    /// Rules that move the first keyword of five fields and retire two others.
    fn custom_rules() -> HeaderFallbackConfig {
        let mut cfg = HeaderFallbackConfig::default();
        set_rule(&mut cfg, HeaderField::CcdTemp, &["DET-TEMP", "CCD-TEMP"], &["MY-TEMP"]);
        set_rule(&mut cfg, HeaderField::Exptime, &["EXPOSURE", "EXPTIME"], &[]);
        set_rule(&mut cfg, HeaderField::Gain, &["MYGAIN", "GAIN"], &[]);
        set_rule(&mut cfg, HeaderField::Instrume, &["CAMERA", "INSTRUME"], &[]);
        set_rule(&mut cfg, HeaderField::Filter, &["FILTER"], &["OLDFILT"]);
        crate::header_fallbacks::validate(&cfg).unwrap()
    }

    #[test]
    fn custom_rules_on_a_freshly_scanned_catalog_preview_zero() {
        for unique in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            rules_fixture(dir.path());
            let rules = custom_rules();
            let conn = scan_root_under(dir.path(), unique, &rules);
            let stored = stored_by_name(&conn);
            assert_eq!(stored.len(), 5);
            // The scan really read the custom order: the first keyword of each moved field.
            assert_eq!(stored["a.fits"].ccd_temp, Some(-5.5));
            assert_eq!(stored["a.fits"].exptime, Some(60.0));
            assert_eq!(stored["a.fits"].gain, Some(77.0));
            let loaded = crate::header_fallbacks::load(&conn).unwrap();
            assert_eq!(loaded, rules);
            assert_eq!(
                preview(&conn, &loaded).unwrap(),
                HeaderFallbackCounts::default(),
                "unique camera root: {unique}"
            );
        }
    }

    /// Scan under `a`, plan and write the changes for `b`, and compare all twelve fields with a
    /// fresh scan of the same files under `b`.
    fn assert_apply_equals_a_scan(a: &HeaderFallbackConfig, b: &HeaderFallbackConfig, unique: bool) {
        let dir = tempfile::tempdir().unwrap();
        rules_fixture(dir.path());
        let applied = scan_root_under(dir.path(), unique, a);
        let roots = ScanRoots::load(&applied).unwrap();
        for row in read_rows(&applied, 0, 100_000).unwrap() {
            write_changes(&applied, row.frame_id, &plan_frame(b, &row, &roots).changes);
        }
        let scanned = scan_root_under(dir.path(), unique, b);
        assert_eq!(stored_by_name(&applied), stored_by_name(&scanned), "unique camera root: {unique}");
        assert_eq!(preview(&applied, b).unwrap(), HeaderFallbackCounts::default(), "unique: {unique}");
    }

    #[test]
    fn applying_the_planned_changes_gives_what_a_scan_under_the_new_rules_stores() {
        let custom = custom_rules();
        // The default order with what the custom one had beyond it retired.
        let back = custom.reset_orders();
        for unique in [false, true] {
            assert_apply_equals_a_scan(&HeaderFallbackConfig::default(), &custom, unique);
            assert_apply_equals_a_scan(&custom, &back, unique);
            assert_apply_equals_a_scan(&custom, &custom, unique);
        }
    }

    #[test]
    fn instrume_follows_the_scan_form_in_the_two_traced_cases() {
        let conn = catalog();
        add_root(&conn, 5, "/cams/five", true);
        add_root(&conn, 6, "/cams/six", false);
        let rules = rules_with(HeaderField::Instrume, &["CAMERA", "INSTRUME"], &[]);
        // (a) plain root: the column `Cam N3` is the header's own INSTRUME, not the catalog's suffix.
        let a = fits_text(&[("INSTRUME", "'Cam N3'"), ("CAMERA", "'Other'")]);
        let col = ResolvedFields { instrume: Some("Cam N3".into()), ..ResolvedFields::default() };
        let id = add_frame(&conn, "/cams/six/a.fits", "FITS", "Light", &col, Some(&a));
        assert_eq!(plan_of(&conn, &rules, id).changes, vec![(HeaderField::Instrume, text("Other"))]);
        // The same in a unique-camera root: the suffix is the root's.
        let col = ResolvedFields { instrume: Some("Cam N5".into()), ..ResolvedFields::default() };
        let id = add_frame(&conn, "/cams/five/a.fits", "FITS", "Light", &col, Some(&a));
        assert_eq!(plan_of(&conn, &rules, id).changes, vec![(HeaderField::Instrume, text("Other N5"))]);
        // (b) plain root: the column `Cam` is what a scan stores for INSTRUME `Cam`; CAMERA now says `Cam N3`.
        let b = fits_text(&[("INSTRUME", "'Cam'"), ("CAMERA", "'Cam N3'")]);
        let col = ResolvedFields { instrume: Some("Cam".into()), ..ResolvedFields::default() };
        let id = add_frame(&conn, "/cams/six/b.fits", "FITS", "Light", &col, Some(&b));
        assert_eq!(plan_of(&conn, &rules, id).changes, vec![(HeaderField::Instrume, text("Cam N3"))]);
        // A master registered without the suffix in a unique root is a candidate exactly: no suffix is added.
        let col = ResolvedFields { instrume: Some("Cam".into()), ..ResolvedFields::default() };
        let c = fits_text(&[("INSTRUME", "'Cam'"), ("CAMERA", "'Cam2'")]);
        let id = add_frame(&conn, "/cams/five/m.fits", "FITS", "Light", &col, Some(&c));
        assert_eq!(plan_of(&conn, &rules, id).changes, vec![(HeaderField::Instrume, text("Cam2"))]);
        let id = add_frame(&conn, "/cams/five/n.fits", "FITS", "Light", &col, Some(&fits_text(&[("INSTRUME", "'Cam'")])));
        assert!(plan_of(&conn, &HeaderFallbackConfig::default(), id).changes.is_empty());
    }

    #[test]
    fn an_xisf_pixel_size_with_a_retired_keyword_in_the_dump_is_left_alone() {
        let conn = catalog();
        // The `<Property>` pixel size (3.76) is in the columns; the dump has only the retired keyword.
        let h = xisf_text(&[("FILTER", "Ha"), ("OLDPIX", "3.76")]);
        let values = ResolvedFields {
            xpixsz: Some(3.76),
            ypixsz: Some(3.76),
            filter: Some("Ha".into()),
            ..ResolvedFields::default()
        };
        let id = add_frame(&conn, "a.xisf", "XISF", "Light", &values, Some(&h));
        let mut rules = HeaderFallbackConfig::default();
        set_rule(&mut rules, HeaderField::Xpixsz, &["XPIXSZ", "PIXSIZE1"], &["OLDPIX"]);
        set_rule(&mut rules, HeaderField::Ypixsz, &["YPIXSZ", "PIXSIZE2"], &["OLDPIX"]);
        let plan = plan_of(&conn, &rules, id);
        assert!(plan.changes.is_empty(), "{:?}", plan.changes);
        assert_eq!(plan.kept, 0);
        assert_eq!(preview(&conn, &rules).unwrap(), HeaderFallbackCounts::default());
        // A FITS frame has no Property: there the retired keyword's value is cleared.
        let fits = fits_text(&[("OLDPIX", "3.76"), ("FILTER", "'Ha'")]);
        let id = add_frame(&conn, "a.fits", "FITS", "Light", &values, Some(&fits));
        let plan = plan_of(&conn, &rules, id);
        assert_eq!(plan.changes, vec![(HeaderField::Xpixsz, None), (HeaderField::Ypixsz, None)]);
    }

    #[test]
    fn a_stored_infinity_reads_as_absent() {
        let conn = catalog();
        let h = fits_text(&[("GAIN", "100")]);
        let id = add_frame(&conn, "a.fits", "FITS", "Light", &ResolvedFields::default(), Some(&h));
        conn.execute("UPDATE frames SET gain = 1e999, offset = -1e999 WHERE id = ?1", params![id]).unwrap();
        let rows = read_rows(&conn, 0, 10).unwrap();
        assert_eq!(rows[0].values.gain, None);
        assert_eq!(rows[0].values.offset, None);
        // Filled from the header like any NULL, and not counted as kept.
        let plan = plan_of(&conn, &HeaderFallbackConfig::default(), id);
        assert_eq!(plan.changes, vec![(HeaderField::Gain, num(100.0))]);
        assert_eq!(plan.kept, 0);
    }

    #[test]
    fn a_blank_text_column_is_cleared_when_the_header_has_no_value() {
        let conn = catalog();
        let h = fits_text(&[("DET-TEMP", "-5")]);
        let values = ResolvedFields {
            ccd_temp: Some(-5.0),
            filter: Some("  ".into()),
            object: Some(String::new()),
            ..ResolvedFields::default()
        };
        let id = add_frame(&conn, "a.fits", "FITS", "Light", &values, Some(&h));
        let plan = plan_of(&conn, &HeaderFallbackConfig::default(), id);
        assert_eq!(plan.changes, vec![(HeaderField::Object, None), (HeaderField::Filter, None)]);
        assert_eq!(plan.kept, 0);
    }

    #[test]
    fn master_library_and_source_sets_are_not_counted_as_regrouped() {
        let conn = catalog();
        let h = fits_text(&[("DET-TEMP", "-5.5")]);
        let none = ResolvedFields::default();
        // A master library set: its frames change, its set is not moved.
        let library = calibration_set(&conn, "MasterDark");
        conn.execute("UPDATE calibration_set SET is_master_library = 1 WHERE id = ?1", params![library]).unwrap();
        let id = add_frame(&conn, "lib.fits", "FITS", "Dark", &none, Some(&h));
        put_in_set(&conn, library, id);
        // The raw set a master was built from.
        let master = calibration_set(&conn, "MasterDark");
        let source = calibration_set(&conn, "Dark");
        conn.execute(
            "INSERT INTO master_provenance (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
             VALUES (?1, ?2, '{}', '[]', 'h', '2026-01-01')",
            params![master, source],
        )
        .unwrap();
        let id = add_frame(&conn, "src.fits", "FITS", "Dark", &none, Some(&h));
        put_in_set(&conn, source, id);
        let counts = preview(&conn, &HeaderFallbackConfig::default()).unwrap();
        assert_eq!(counts.frames_changed, 2);
        assert_eq!(counts.sets_regrouped, 0);
        // A plain raw set beside them is counted.
        let raw = calibration_set(&conn, "Dark");
        let id = add_frame(&conn, "raw.fits", "FITS", "Dark", &none, Some(&h));
        put_in_set(&conn, raw, id);
        assert_eq!(preview(&conn, &HeaderFallbackConfig::default()).unwrap().sets_regrouped, 1);
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
