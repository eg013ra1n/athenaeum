//! Rebuilt copies of one master (MCP plan, Task 11): masters in the calibration library that were built twice,
//! typically the `_2` / `_3` collision suffix of a master written beside its earlier copy. They are not duplicates by
//! any hash, so the Duplicates page does not list them.
//!
//! A master here is a frame the catalog itself flags `is_master = 1` (the scanner sets it from IMAGETYP or the file
//! name) in a folder under a scan root of kind `calibration_library`, and not in the Black Hole. Two masters are the
//! same master when camera, type, gain, offset, binning, width and height are equal exactly, exposure agrees to 1 ms,
//! CCD temperature to 0.1 degrees C, the filter after `normalize_value`, and the shooting date is the same night.
//!
//! The finder only reads. Which copy stays is never decided here: a copy a calibration link points at (directly, or
//! through the sub-calibration chain a master flat's dark is part of) says so in `linked_lights`, and the caller keeps
//! it.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::catalog_query::{fetch_scoped_frames, file_line, FileLine};
use crate::db::FRAME_NOT_IN_BLACK_HOLE_SQL;
use crate::scope::{normalize_value, ScopeFilter};

/// The scan-root kind that marks the calibration library.
const LIBRARY_KIND: &str = "calibration_library";

/// One copy of a rebuilt master.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RebuiltCopy {
    pub file_id: i64,
    /// `file:<file id>`, file name and folder, and the header values.
    pub line: FileLine,
    /// The calibration set that holds this copy (its master set when it has one).
    pub calibration_set_id: Option<i64>,
    /// How many frames calibration links point from at this copy's set, directly or through the sub-calibration
    /// chain (a master dark behind a master flat counts the lights the flat serves). 0: nothing uses this copy.
    pub linked_lights: usize,
}

/// The copies of one master, two or more.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RebuiltMasterGroup {
    /// `rebuilt:<lowest file id of the group>`.
    pub handle: String,
    /// What the copies share, in words: `ZWO ASI2600MC Duo, MasterDark, 300 s, -10.0 °C, gain 100, ...`.
    pub params_label: String,
    /// Ordered by file id.
    pub copies: Vec<RebuiltCopy>,
}

/// What makes two masters the same master.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SameMaster {
    camera: Option<String>,
    kind: Option<String>,
    gain: Option<u64>,
    offset: Option<u64>,
    binning: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
    exposure_ms: Option<i64>,
    temp_tenths: Option<i64>,
    filter: String,
    night: Option<String>,
}

/// An exact comparison key for a float (`-0.0` and `0.0` are one value).
fn exact(v: Option<f64>) -> Option<u64> {
    v.map(|x| (x + 0.0).to_bits())
}

/// The masters of the calibration library the groups are made of, as a predicate on `fr.id` (the scope filter's
/// shape). Inside it `f` is `frames`.
fn library_masters_sql() -> String {
    format!("fr.id IN (SELECT f.id FROM frames f WHERE COALESCE(f.is_master, 0) = 1 AND {FRAME_NOT_IN_BLACK_HOLE_SQL})")
}

/// The calibration set holding this frame: its master set when it has one, else the lowest id.
fn set_of_frame(conn: &Connection, frame_id: i64) -> Result<Option<i64>> {
    conn.query_row(
        "SELECT cs.id FROM calibration_set_frames csf JOIN calibration_set cs ON cs.id = csf.set_id \
         WHERE csf.frame_id = ?1 ORDER BY COALESCE(cs.is_master_library, 0) DESC, cs.id LIMIT 1",
        params![frame_id],
        |r| r.get(0),
    )
    .optional()
    .with_context(|| format!("read the calibration set of frame {frame_id}"))
}

/// The distinct frames that calibration links point from at `set_id`, or at a set that uses it as a sub-calibration
/// (the chain `get_calibration_set_consumers` walks).
fn linked_frames(conn: &Connection, set_id: i64) -> Result<usize> {
    let n: i64 = conn
        .query_row(
            "WITH RECURSIVE consumer_chain(set_id) AS ( \
                 SELECT ?1 \
                 UNION \
                 SELECT cstf.source_id FROM calibration_set_to_frames cstf \
                   JOIN consumer_chain cc ON cstf.calibration_set_id = cc.set_id \
                  WHERE cstf.source_type = 'calibration_set' \
             ) \
             SELECT COUNT(DISTINCT cstf.source_id) FROM calibration_set_to_frames cstf \
               JOIN consumer_chain cc ON cstf.calibration_set_id = cc.set_id \
              WHERE cstf.source_type = 'frame'",
            params![set_id],
            |r| r.get(0),
        )
        .with_context(|| format!("count the frames linked to calibration set {set_id}"))?;
    Ok(usize::try_from(n).unwrap_or(0))
}

/// How many frames calibration links point from at the master in `file_id`: through the set that holds it, directly
/// or through the sub-calibration chain (the rule `RebuiltCopy::linked_lights` reports). 0 when the file has no frame
/// or its frame is in no calibration set. Any master file, a duplicate group's included.
pub fn master_link_count(conn: &Connection, file_id: i64) -> Result<usize> {
    let frame_id: Option<i64> = conn
        .query_row("SELECT id FROM frames WHERE file_id = ?1 ORDER BY id LIMIT 1", params![file_id], |r| r.get(0))
        .optional()
        .with_context(|| format!("read the frame of file {file_id}"))?;
    let Some(frame_id) = frame_id else { return Ok(0) };
    match set_of_frame(conn, frame_id)? {
        Some(set) => linked_frames(conn, set),
        None => Ok(0),
    }
}

/// The shared parameters of a group, in words (the first copy's, which equal the others' to the key's precision).
fn params_label(line: &FileLine) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(c) = &line.instrume {
        parts.push(c.clone());
    }
    if let Some(t) = &line.imagetyp {
        parts.push(t.clone());
    }
    if let Some(e) = line.exptime_s {
        parts.push(format!("{} s", (e * 1000.0).round() / 1000.0));
    }
    if let Some(t) = line.ccd_temp {
        parts.push(format!("{t:.1} \u{b0}C"));
    }
    if let Some(g) = line.gain {
        parts.push(format!("gain {g}"));
    }
    if let Some(o) = line.offset {
        parts.push(format!("offset {o}"));
    }
    if let Some(b) = &line.binning {
        parts.push(format!("bin {b}"));
    }
    if let (Some(w), Some(h)) = (line.width, line.height) {
        parts.push(format!("{w}x{h} px"));
    }
    if let Some(f) = line.filter.as_deref().filter(|f| !f.trim().is_empty()) {
        parts.push(format!("filter {f}"));
    }
    if let Some(n) = &line.night {
        parts.push(format!("night {n}"));
    }
    parts.join(", ")
}

/// The masters of the calibration library that exist in more than one copy, optionally narrowed by a resolved
/// scope's `filter` (its `state` too: a filter that asks for the Black Hole still never returns a Black Hole copy
/// here). Groups come in order of their lowest file id.
pub fn find_rebuilt_master_copies(
    conn: &Connection,
    filter: Option<&ScopeFilter>,
) -> Result<Vec<RebuiltMasterGroup>> {
    let mut sql = library_masters_sql();
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(f) = filter {
        sql = format!("({sql}) AND ({})", f.sql);
        params.extend(f.params.iter().cloned());
    }
    let masters = fetch_scoped_frames(conn, &ScopeFilter { sql, params })
        .context("read the masters of the calibration library")?;

    let mut groups: BTreeMap<SameMaster, Vec<(i64, i64, FileLine)>> = BTreeMap::new();
    for sf in &masters {
        if !matches!(&sf.scan_root, Some((_, kind)) if kind == LIBRARY_KIND) {
            continue;
        }
        let (Some(frame), Some(file_id)) = (sf.file.frame.as_ref(), sf.file.file.id) else { continue };
        let line = file_line(sf);
        let key = SameMaster {
            camera: frame.instrume.clone(),
            kind: line.imagetyp.clone(),
            gain: exact(frame.gain),
            offset: exact(frame.offset),
            binning: frame.binning.clone(),
            width: frame.naxis1,
            height: frame.naxis2,
            exposure_ms: frame.exptime.map(|e| (e * 1000.0).round() as i64),
            temp_tenths: frame.ccd_temp.map(|t| (t * 10.0).round() as i64),
            filter: normalize_value(frame.filter.as_deref().unwrap_or("")),
            night: sf.night.clone(),
        };
        groups.entry(key).or_default().push((file_id, frame.id.unwrap_or_default(), line));
    }

    let mut out = Vec::new();
    for (_, mut members) in groups {
        if members.len() < 2 {
            continue;
        }
        members.sort_by_key(|(file_id, _, _)| *file_id);
        let handle = format!("rebuilt:{}", members[0].0);
        let label = params_label(&members[0].2);
        let mut copies = Vec::with_capacity(members.len());
        for (file_id, frame_id, line) in members {
            let calibration_set_id = set_of_frame(conn, frame_id)?;
            let linked_lights = master_link_count(conn, file_id)?;
            copies.push(RebuiltCopy { file_id, line, calibration_set_id, linked_lights });
        }
        out.push(RebuiltMasterGroup { handle, params_label: label, copies });
    }
    out.sort_by_key(|g| g.copies[0].file_id);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::models::{File, FileFormat, Frame, ImageType};
    use chrono::Utc;

    fn catalog() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        db::schema::init_db(&conn).expect("schema");
        conn
    }

    /// The library root and a normal root, as paths this platform spells.
    fn lib_dir() -> String {
        if cfg!(windows) { "C:\\library\\darks".to_string() } else { "/library/darks".to_string() }
    }

    fn lib_root() -> String {
        if cfg!(windows) { "C:\\library".to_string() } else { "/library".to_string() }
    }

    fn other_dir() -> String {
        if cfg!(windows) { "C:\\elsewhere".to_string() } else { "/elsewhere".to_string() }
    }

    struct Master<'a> {
        dir: String,
        name: &'a str,
        temp: f64,
        exptime: f64,
        is_master: bool,
    }

    impl<'a> Master<'a> {
        fn dark(name: &'a str) -> Self {
            Master { dir: lib_dir(), name, temp: -10.0, exptime: 300.0, is_master: true }
        }
    }

    /// Returns (file id, frame id).
    fn add(conn: &Connection, m: &Master) -> (i64, i64) {
        let sep = std::path::MAIN_SEPARATOR;
        let file_id = db::insert_file(
            conn,
            &File {
                id: None,
                path: format!("{}{sep}{}", m.dir, m.name),
                filename: m.name.to_string(),
                size: 100,
                modified_at: Utc::now(),
                format: FileFormat::XISF,
                created_at: Utc::now(),
                content_hash: None,
                archived_in_operation: None,
                archive_zip_path: None,
                archive_path_in_zip: None,
                uuid: None,
                updated_at: None,
            },
        )
        .expect("file");
        let frame_id = db::insert_frame(
            conn,
            &Frame {
                file_id,
                instrume: Some("Cam".to_string()),
                imagetyp: Some(ImageType::MasterDark),
                is_master: m.is_master,
                exptime: Some(m.exptime),
                ccd_temp: Some(m.temp),
                gain: Some(100.0),
                offset: Some(50.0),
                binning: Some("1x1".to_string()),
                naxis1: Some(6248),
                naxis2: Some(4176),
                date_obs: Some(
                    chrono::DateTime::parse_from_rfc3339("2024-10-05T22:00:00Z").expect("date").with_timezone(&Utc),
                ),
                ..Default::default()
            },
        )
        .expect("frame");
        (file_id, frame_id)
    }

    /// A master set holding `frame_id`, linked from `lights` light frames.
    fn linked_set(conn: &Connection, frame_id: i64, lights: usize) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, is_master_library, instrume) VALUES ('MasterDark', '2024-10-05', 1, 'Cam')",
            [],
        )
        .expect("set");
        let set = conn.last_insert_rowid();
        conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)", [set, frame_id])
            .expect("member");
        for n in 0..lights {
            let light_file = db::insert_file(
                conn,
                &File {
                    id: None,
                    path: format!("{}{}light{n}.fits", other_dir(), std::path::MAIN_SEPARATOR),
                    filename: format!("light{n}.fits"),
                    size: 1,
                    modified_at: Utc::now(),
                    format: FileFormat::FITS,
                    created_at: Utc::now(),
                    content_hash: None,
                    archived_in_operation: None,
                    archive_zip_path: None,
                    archive_path_in_zip: None,
                    uuid: None,
                    updated_at: None,
                },
            )
            .expect("light file");
            let light = db::insert_frame(
                conn,
                &Frame { file_id: light_file, imagetyp: Some(ImageType::Light), ..Default::default() },
            )
            .expect("light frame");
            conn.execute(
                "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type, matched_at) \
                 VALUES (?1, 'frame', ?2, 'Dark', '2024-10-05T00:00:00Z')",
                [light, set],
            )
            .expect("link");
        }
        set
    }

    #[test]
    fn rebuilt_copies_found_with_suffixes() {
        let conn = catalog();
        db::upsert_scan_root(&conn, &lib_root(), "calibration_library").expect("root");
        let (first, first_frame) = add(&conn, &Master::dark("MasterDark_300s.xisf"));
        let (second, _) = add(&conn, &Master::dark("MasterDark_300s_2.xisf"));
        let (third, _) = add(&conn, &Master::dark("MasterDark_300s_3.xisf"));
        // A master of other parameters in the same folder is not a copy of these.
        add(&conn, &Master { exptime: 60.0, ..Master::dark("MasterDark_60s.xisf") });
        let set = linked_set(&conn, first_frame, 3);

        let groups = find_rebuilt_master_copies(&conn, None).expect("find");
        assert_eq!(groups.len(), 1, "{groups:?}");
        let g = &groups[0];
        assert_eq!(g.handle, format!("rebuilt:{first}"));
        let ids: Vec<i64> = g.copies.iter().map(|c| c.file_id).collect();
        assert_eq!(ids, vec![first, second, third]);
        assert_eq!(g.copies[0].line.handle, format!("file:{first}"));
        assert_eq!(g.copies[0].line.file_name, "MasterDark_300s.xisf");
        assert_eq!(g.copies[0].line.folder, lib_dir());
        assert_eq!(g.copies[0].calibration_set_id, Some(set));
        assert_eq!(g.copies[0].linked_lights, 3);
        assert_eq!(g.copies[1].linked_lights, 0);
        assert_eq!(g.copies[2].linked_lights, 0);
        assert_eq!(g.copies[1].calibration_set_id, None);
        assert!(g.params_label.contains("300 s") && g.params_label.contains("-10.0"), "{}", g.params_label);
    }

    #[test]
    fn a_copy_linked_through_a_flat_set_counts() {
        let conn = catalog();
        db::upsert_scan_root(&conn, &lib_root(), "calibration_library").expect("root");
        let (first, first_frame) = add(&conn, &Master::dark("MasterDark_300s.xisf"));
        let (second, _) = add(&conn, &Master::dark("MasterDark_300s_2.xisf"));
        let dark_set = linked_set(&conn, first_frame, 0);
        // A flat set uses the dark set as its sub-calibration; one light is linked to the flat set only.
        conn.execute(
            "INSERT INTO calibration_set (imagetyp, date, is_master_library, instrume) VALUES ('MasterFlat', '2024-10-05', 1, 'Cam')",
            [],
        )
        .expect("flat set");
        let flat_set = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type, matched_at) \
             VALUES (?1, 'calibration_set', ?2, 'Dark', '2024-10-05T00:00:00Z')",
            [flat_set, dark_set],
        )
        .expect("flat uses dark");
        let light_file = db::insert_file(
            &conn,
            &File {
                id: None,
                path: format!("{}{}light.fits", other_dir(), std::path::MAIN_SEPARATOR),
                filename: "light.fits".to_string(),
                size: 1,
                modified_at: Utc::now(),
                format: FileFormat::FITS,
                created_at: Utc::now(),
                content_hash: None,
                archived_in_operation: None,
                archive_zip_path: None,
                archive_path_in_zip: None,
                uuid: None,
                updated_at: None,
            },
        )
        .expect("light file");
        let light = db::insert_frame(
            &conn,
            &Frame { file_id: light_file, imagetyp: Some(ImageType::Light), ..Default::default() },
        )
        .expect("light frame");
        conn.execute(
            "INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type, matched_at) \
             VALUES (?1, 'frame', ?2, 'Flat', '2024-10-05T00:00:00Z')",
            [light, flat_set],
        )
        .expect("light uses flat");

        assert!(master_link_count(&conn, first).expect("count") > 0);
        assert_eq!(master_link_count(&conn, second).expect("count"), 0);
        let groups = find_rebuilt_master_copies(&conn, None).expect("find");
        assert_eq!(groups.len(), 1, "{groups:?}");
        assert!(groups[0].copies[0].linked_lights > 0, "{groups:?}");
        assert_eq!(groups[0].copies[0].linked_lights, master_link_count(&conn, first).expect("count"));
    }

    #[test]
    fn different_temperature_is_another_master() {
        let conn = catalog();
        db::upsert_scan_root(&conn, &lib_root(), "calibration_library").expect("root");
        add(&conn, &Master { temp: -10.0, ..Master::dark("MasterDark_a.xisf") });
        add(&conn, &Master { temp: -15.0, ..Master::dark("MasterDark_b.xisf") });
        assert!(find_rebuilt_master_copies(&conn, None).expect("find").is_empty());

        // Within a tenth of a degree they are one master.
        add(&conn, &Master { temp: -10.04, ..Master::dark("MasterDark_c.xisf") });
        let groups = find_rebuilt_master_copies(&conn, None).expect("find");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].copies.len(), 2);
    }

    #[test]
    fn only_library_masters_outside_the_black_hole_count() {
        let conn = catalog();
        db::upsert_scan_root(&conn, &lib_root(), "calibration_library").expect("root");
        db::upsert_scan_root(&conn, &other_dir(), "normal").expect("root");
        let (_, _) = add(&conn, &Master::dark("MasterDark_a.xisf"));
        // The same master, outside the library: not counted.
        add(&conn, &Master { dir: other_dir(), ..Master::dark("MasterDark_b.xisf") });
        // The same master, not flagged as a master: not counted.
        add(&conn, &Master { is_master: false, ..Master::dark("MasterDark_c.xisf") });
        assert!(find_rebuilt_master_copies(&conn, None).expect("find").is_empty());

        // A second library copy makes a group; sending it to the Black Hole dissolves it again.
        let (second, _) = add(&conn, &Master::dark("MasterDark_d.xisf"));
        assert_eq!(find_rebuilt_master_copies(&conn, None).expect("find").len(), 1);
        conn.execute(
            "INSERT INTO black_hole (file_id, from_where, moved_at, original_path) VALUES (?1, 'test', 'now', 'x')",
            [second],
        )
        .expect("black hole");
        assert!(find_rebuilt_master_copies(&conn, None).expect("find").is_empty());
    }
}
