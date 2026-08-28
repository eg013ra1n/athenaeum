//! Direct master registration (spec §3 step 4).
//!
//! Phase 2 core invariant: a master built in-app gets DB rows IDENTICAL to
//! scanner ingestion BY CONSTRUCTION. The just-written master FITS file is
//! parsed with the SAME `fits_parser::parse_fits_with_header` the scanner
//! uses, inserted with the SAME `crate::db::{insert_file, insert_frame,
//! insert_fits_header}`, and turned into a 1:1 master `calibration_set` by
//! the SAME `scan_integration::create_master_sets_from_frames` the scanner
//! calls when it discovers a master frame on disk. Only after that do we add
//! the Athenaeum-specific bookkeeping the scanner never does on its own:
//! `master_provenance`, relinking existing consumers from the raw set to the
//! master, and marking the raw set superseded.

use crate::calibration::scan_integration::create_master_sets_from_frames;
use crate::db::master_provenance::{self, MasterProvenance};
use crate::fits_parser::parse_fits_with_header;
use crate::models::{File, FileFormat};
use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use std::path::Path;

/// Result of a successful [`register_master`] call — enough to let the
/// caller (Task 12's `api::masters` orchestration) report what happened
/// without re-querying the DB.
pub struct MasterRegistration {
    pub master_set_id: i64,
    pub master_frame_id: i64,
    pub master_file_id: i64,
    pub relinked_links: usize,
}

/// xxh3-of-sorted-member-uuids — the stable member-identity hash stamped as
/// `ATH_HSH` in the master's own header (see
/// `calibration_library::headers::build_master_cards`) and stored in
/// `master_provenance.member_hash`.
///
/// Content hashes (`files.content_hash`) are only computed when duplicate
/// detection opts into hashing during a scan, so they may be `NULL` for any
/// given member frame — an identity hash built on top of them would be
/// unreliable. `frames.uuid` is a Phase-1 invariant: every frame row gets one
/// via an `AFTER INSERT` trigger the moment it's inserted, so it's always
/// present and is the right foundation for "did this exact set of frames
/// build this master" identity.
pub fn member_hash(conn: &Connection, source_set_id: i64) -> Result<(String, Vec<String>)> {
    let mut stmt = conn.prepare(
        "SELECT f.uuid FROM calibration_set_frames csf
         JOIN frames f ON f.id = csf.frame_id
         WHERE csf.set_id = ?1 ORDER BY f.uuid",
    )?;
    let uuids: Vec<String> = stmt
        .query_map([source_set_id], |r| r.get::<_, Option<String>>(0))?
        .filter_map(|r| r.ok().flatten())
        .collect();
    let joined = uuids.join(",");
    let hash = format!("{:016x}", xxhash_rust::xxh3::xxh3_64(joined.as_bytes()));
    Ok((hash, uuids))
}

/// Register a just-written master FITS file as the replacement for
/// `source_set_id`.
///
/// One transaction covers every DB mutation: `files`/`frames`/`fits_header`
/// rows for the master, the 1:1 master `calibration_set`,
/// `master_provenance`, the relink of every existing consumer of the raw set
/// onto the master, and marking the raw set superseded. A failure at any
/// step (including a `create_master_sets_from_frames` count mismatch) rolls
/// the whole thing back — the DB never observes a half-registered master.
///
/// Everything that can fail *before* any DB mutation (the raw set already
/// superseded, the master file missing on disk, the file failing to parse,
/// or parsing to a non-Master `IMAGETYP`) is checked before the transaction
/// opens, so those failures leave the DB completely untouched too — not just
/// rolled back, but never written to in the first place.
pub fn register_master(
    conn: &Connection,
    source_set_id: i64,
    master_path: &Path,
    recipe_json: &str,
) -> Result<MasterRegistration> {
    // Cheap guard first: a set can only be registered once. Re-running
    // register_master on an already-superseded set is a caller bug (it
    // should be calling the Task 13 rebuild path instead), not a silent
    // no-op — surfacing it here avoids creating a second master set for the
    // same raw frames.
    // TODO: this pre-transaction SELECT is a benign TOCTOU window for
    // concurrent same-set registrations; Task 12's per-set duplicate guard
    // serializes this in practice.
    let already_superseded: Option<i64> = conn
        .query_row(
            "SELECT superseded_by_set_id FROM calibration_set WHERE id = ?1",
            [source_set_id],
            |r| r.get(0),
        )
        .context("source calibration set not found")?;
    if let Some(master_id) = already_superseded {
        bail!(
            "calibration set {source_set_id} is already superseded by master set {master_id} — \
             use the rebuild path instead of registering a new master"
        );
    }

    // Parse with file_id=0 (matches the scanner's own early-parse
    // optimization for the moved-file check, scanner/mod.rs process_file) so
    // a missing/unparseable file or a header that doesn't actually carry a
    // Master IMAGETYP is caught before any DB write happens. file_id is
    // patched to the real value once the files row exists, below.
    let (mut frame, header_text) = parse_fits_with_header(master_path, 0)
        .with_context(|| format!("freshly written master failed to parse: {}", master_path.display()))?;

    let is_master_type = frame.imagetyp.as_ref().map(|t| t.is_master()).unwrap_or(false);
    if !is_master_type {
        bail!(
            "written master lacks a Master IMAGETYP — header consolidation bug (parsed imagetyp: {:?})",
            frame.imagetyp
        );
    }
    // Matches insert_frame's / the scanner's own storage form (Debug of
    // ImageType, e.g. "MasterDark") — see scanner/mod.rs:796-799.
    let imagetyp_str = format!("{:?}", frame.imagetyp.as_ref().unwrap());

    // file metadata — same field sourcing as scanner::process_file.
    let meta = std::fs::metadata(master_path)
        .with_context(|| format!("master file not found: {}", master_path.display()))?;
    let modified_at = chrono::DateTime::<chrono::Utc>::from(
        meta.modified().context("reading master file mtime")?,
    );
    let path_str = master_path.to_string_lossy().to_string();
    let filename = master_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("master.fits")
        .to_string();
    let format = match master_path.extension().map(|e| e.to_string_lossy().to_lowercase()) {
        Some(ext) if ext == "xisf" => FileFormat::XISF,
        _ => FileFormat::FITS,
    };
    // member_hash reads calibration_set_frames/frames for the RAW set — read
    // before the transaction opens (no mutation involved), and before the
    // raw set's frames get relinked/superseded below.
    let (hash, member_uuids) = member_hash(conn, source_set_id)?;

    let tx = conn.unchecked_transaction()?;

    // 1. files row — crate::db::insert_file, same helper the scanner uses.
    let file = File {
        id: None,
        path: path_str,
        filename,
        size: meta.len() as i64,
        modified_at,
        format,
        created_at: chrono::Utc::now(),
        content_hash: None,
        archived_in_operation: None,
        archive_zip_path: None,
        archive_path_in_zip: None,
        uuid: None,
        updated_at: None,
    };
    let file_id = crate::db::insert_file(&tx, &file)?;

    // 2. frames row + stored header — same helpers, same parsed Frame.
    frame.file_id = file_id;
    let frame_id = crate::db::insert_frame(&tx, &frame)?;
    crate::db::insert_fits_header(&tx, file_id, &header_text)?;

    // 3. 1:1 master calibration_set — the scanner's own function, so the row
    // shape matches scanner-ingested masters by construction.
    let created = create_master_sets_from_frames(&tx, &[frame_id], &imagetyp_str)?;
    if created != 1 {
        bail!("expected exactly one master calibration_set to be created, got {created}");
    }
    let master_set_id: i64 = tx.query_row(
        "SELECT set_id FROM calibration_set_frames WHERE frame_id = ?1",
        [frame_id],
        |r| r.get(0),
    )?;

    // 4. provenance.
    master_provenance::insert(
        &tx,
        &MasterProvenance {
            master_set_id,
            source_set_id: Some(source_set_id),
            recipe_json: recipe_json.to_string(),
            member_frame_uuids: serde_json::to_string(&member_uuids)?,
            member_hash: hash,
            created_at: chrono::Utc::now().to_rfc3339(),
        },
    )?;

    // 5. Relink every existing consumer of the raw set onto the master: both
    // light-frame links (source_type='frame', e.g. a Light that was matched
    // to this Dark) AND sub-calibration links from another calibration_set
    // (source_type='calibration_set', e.g. a Flat set whose Dark sub-cal
    // pointed at this raw dark set). A single UPDATE keyed on
    // calibration_set_id covers both source_types identically —
    // is_manual_override and match_score are untouched, since the UPDATE
    // only ever assigns calibration_set_id.
    //
    // This can't violate calibration_set_to_frames's
    // UNIQUE(source_id, source_type, calibration_type): that key doesn't
    // include calibration_set_id at all, so repointing which set a
    // (source_id, source_type, calibration_type) row targets never creates
    // a duplicate — we're rewriting existing rows in place, not inserting
    // new ones, and each existing row already satisfies the unique
    // constraint (there is at most one link per
    // source/type/calibration_type regardless of which set it points at).
    let relinked_links = tx.execute(
        "UPDATE calibration_set_to_frames SET calibration_set_id = ?1 WHERE calibration_set_id = ?2",
        rusqlite::params![master_set_id, source_set_id],
    )?;

    // 6. Supersede the raw set.
    tx.execute(
        "UPDATE calibration_set SET superseded_by_set_id = ?1 WHERE id = ?2",
        rusqlite::params![master_set_id, source_set_id],
    )?;

    tx.commit()?;

    Ok(MasterRegistration {
        master_set_id,
        master_frame_id: frame_id,
        master_file_id: file_id,
        relinked_links,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::keywords::{Bayer, FrameKind, HeaderBuilder};
    use crate::fits_writer::write_fits_f32;
    use rusqlite::Connection;

    /// Writes a parseable dark frame and registers it in files/frames like the
    /// scanner would (via SQL, since scan_directory needs a full root walk).
    fn seed_source_set(conn: &Connection, dir: &std::path::Path) -> i64 {
        crate::db::schema::init_db(conn).unwrap();
        conn.execute(
            "INSERT INTO calibration_set
             (imagetyp, exptime, ccd_temp, gain, offset, binning, instrume, date,
              date_start, date_end, temp_min, temp_max, frame_count)
             VALUES ('Dark', 300.0, -10.0, 100.0, 50.0, '1x1', 'TestCam', '2026-06-28',
              '2026-06-28T20:00:00Z', '2026-06-28T22:00:00Z', -10.5, -9.5, 3)",
            [],
        ).unwrap();
        let set_id = conn.last_insert_rowid();
        for i in 0..3 {
            let p = dir.join(format!("raw{i}.fits"));
            let cards = HeaderBuilder::new(FrameKind::Dark)
                .instrume("TestCam").exptime(300.0).gain(100).offset(50)
                .binning(1, 1).ccd_temp(-10.0)
                .build().unwrap();
            write_fits_f32(&p, 8, 8, 1, &vec![100.0; 64], &cards).unwrap();
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 100, '2026-06-28', 'FITS')",
                rusqlite::params![p.to_string_lossy(), format!("raw{i}.fits")],
            ).unwrap();
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp, instrume, exptime, gain, offset, binning, ccd_temp, date_obs)
                 VALUES (?1, 'Dark', 'TestCam', 300.0, 100.0, 50.0, '1x1', -10.0, '2026-06-28T21:00:00Z')",
                rusqlite::params![file_id],
            ).unwrap();
            let frame_id = conn.last_insert_rowid();
            conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                rusqlite::params![set_id, frame_id]).unwrap();
        }
        set_id
    }

    /// A light frame linked to the raw set — the relink subject.
    fn seed_light_link(conn: &Connection, raw_set_id: i64) -> i64 {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES ('/l/light.fits', 'light.fits', 100, '2026-06-28', 'FITS')", []).unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp, instrume, exptime) VALUES (?1, 'Light', 'TestCam', 300.0)",
            [file_id]).unwrap();
        let light_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, match_score, is_manual_override)
             VALUES (?1, 'frame', ?2, 'Dark', 0.9, 1)",
            rusqlite::params![light_id, raw_set_id]).unwrap();
        light_id
    }

    /// A Flat calibration set whose sub-calibration Dark link targets the
    /// raw dark set (source_type='calibration_set') — the OTHER half of the
    /// relink guarantee. Returns the flat set's id.
    fn seed_subcal_link(conn: &Connection, raw_set_id: i64) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set
             (imagetyp, exptime, ccd_temp, gain, offset, binning, instrume, date,
              date_start, date_end, temp_min, temp_max, frame_count)
             VALUES ('Flat', 2.0, -10.0, 100.0, 50.0, '1x1', 'TestCam', '2026-06-28',
              '2026-06-28T19:00:00Z', '2026-06-28T19:30:00Z', -10.2, -9.8, 5)",
            [],
        ).unwrap();
        let flat_set_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, match_score, is_manual_override)
             VALUES (?1, 'calibration_set', ?2, 'Dark', 0.8, 0)",
            rusqlite::params![flat_set_id, raw_set_id]).unwrap();
        flat_set_id
    }

    /// Carries the Bayer geometry a consolidated master header now stamps
    /// (real phase + row order, not fabricated zeros) so the round-trip below
    /// pins that those cards survive write -> re-parse -> `frames` columns.
    fn write_master(dir: &std::path::Path) -> std::path::PathBuf {
        let p = dir.join("master_dark.fits");
        let cards = HeaderBuilder::new(FrameKind::MasterDark)
            .instrume("TestCam").exptime(300.0).gain(100).offset(50)
            .binning(1, 1).ccd_temp(-10.0)
            .bayer(Bayer::Rggb, 1, 0).roworder("BOTTOM-UP")
            .build().unwrap();
        write_fits_f32(&p, 8, 8, 1, &vec![100.0; 64], &cards).unwrap();
        p
    }

    #[test]
    fn register_master_full_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        let raw = seed_source_set(&conn, dir.path());
        let light = seed_light_link(&conn, raw);
        let flat_set = seed_subcal_link(&conn, raw);
        let master_path = write_master(dir.path());

        let reg = register_master(&conn, raw, &master_path, r#"{"combine":"median"}"#).unwrap();

        // 1:1 master set, is_master_library, correct imagetyp
        let (imagetyp, is_master, count): (String, i64, i64) = conn.query_row(
            "SELECT imagetyp, is_master_library, frame_count FROM calibration_set WHERE id=?1",
            [reg.master_set_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((imagetyp.as_str(), is_master, count), ("MasterDark", 1, 1));

        // frames row is_master, files row points at the library file
        let (is_master_frame,): (i64,) = conn.query_row(
            "SELECT is_master FROM frames WHERE id=?1", [reg.master_frame_id], |r| Ok((r.get(0)?,))).unwrap();
        assert_eq!(is_master_frame, 1);
        let path: String = conn.query_row(
            "SELECT path FROM files WHERE id=?1", [reg.master_file_id], |r| r.get(0)).unwrap();
        assert_eq!(path, master_path.to_string_lossy());

        // The Bayer cards the header consolidator now stamps must land in the
        // master's OWN frame row, verbatim — registration re-parses the file it
        // just wrote, so a phase of 1 has to come back as 1 (a 0 here would be
        // the fabrication bug reappearing one layer down).
        let (bayerpat, xbayroff, ybayroff, roworder): (
            Option<String>, Option<i64>, Option<i64>, Option<String>,
        ) = conn.query_row(
            "SELECT bayerpat, xbayroff, ybayroff, roworder FROM frames WHERE id=?1",
            [reg.master_frame_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).unwrap();
        assert_eq!(bayerpat.as_deref(), Some("RGGB"));
        assert_eq!(xbayroff, Some(1), "real phase must survive the write/re-parse round trip");
        assert_eq!(ybayroff, Some(0));
        assert_eq!(roworder.as_deref(), Some("BOTTOM-UP"));

        // relink: the light's Dark link now points at the master, manual flag preserved
        let (set_id, manual): (i64, i64) = conn.query_row(
            "SELECT calibration_set_id, is_manual_override FROM calibration_set_to_frames
             WHERE source_id=?1 AND source_type='frame' AND calibration_type='Dark'",
            [light], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((set_id, manual), (reg.master_set_id, 1));

        // relink also covers sub-calibration links: the flat set's Dark
        // sub-cal (source_type='calibration_set') now points at the master
        // too, with match_score / is_manual_override untouched.
        let (sub_set_id, sub_score, sub_manual): (i64, f64, i64) = conn.query_row(
            "SELECT calibration_set_id, match_score, is_manual_override FROM calibration_set_to_frames
             WHERE source_id=?1 AND source_type='calibration_set' AND calibration_type='Dark'",
            [flat_set], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!(sub_set_id, reg.master_set_id, "sub-cal link must repoint to the master");
        assert!((sub_score - 0.8).abs() < 1e-9, "sub-cal match_score must be preserved");
        assert_eq!(sub_manual, 0, "sub-cal is_manual_override must be preserved");

        // both link kinds were repointed by the single relink UPDATE
        assert_eq!(reg.relinked_links, 2);

        // supersede + provenance
        let sup: Option<i64> = conn.query_row(
            "SELECT superseded_by_set_id FROM calibration_set WHERE id=?1", [raw], |r| r.get(0)).unwrap();
        assert_eq!(sup, Some(reg.master_set_id));
        let prov = crate::db::master_provenance::get(&conn, reg.master_set_id).unwrap().unwrap();
        assert_eq!(prov.source_set_id, Some(raw));
        let uuids: Vec<String> = serde_json::from_str(&prov.member_frame_uuids).unwrap();
        assert_eq!(uuids.len(), 3);
    }

    #[test]
    fn register_is_not_repeatable_on_superseded_set() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        let raw = seed_source_set(&conn, dir.path());
        let master_path = write_master(dir.path());
        register_master(&conn, raw, &master_path, "{}").unwrap();
        let again = register_master(&conn, raw, &master_path, "{}");
        assert!(again.is_err(), "second registration must be rejected");
    }

    /// Normalize one SQLite column value to a comparable `Option<String>`
    /// regardless of its storage class (TEXT vs INTEGER vs REAL) — lets the
    /// pinning test below diff `frames`/`calibration_set` rows produced by
    /// two different code paths (direct registration vs scanner ingestion)
    /// column-by-column without caring whether a given column happens to be
    /// stored as an integer or a real.
    fn col_to_string(v: rusqlite::types::ValueRef) -> Option<String> {
        use rusqlite::types::ValueRef;
        match v {
            ValueRef::Null => None,
            ValueRef::Integer(i) => Some(i.to_string()),
            ValueRef::Real(f) => Some(f.to_string()),
            ValueRef::Text(t) => Some(String::from_utf8_lossy(t).to_string()),
            ValueRef::Blob(_) => None,
        }
    }

    #[test]
    fn direct_registration_matches_scanner_ingestion() {
        // Spec §10 pinning test: register the SAME master file both ways and
        // diff the rows column-by-column (ids/uuids/timestamps excluded).
        let dir = tempfile::tempdir().unwrap();
        let master = write_master(dir.path());

        // Path A: direct registration (fresh DB + fresh source set).
        let conn_a = Connection::open_in_memory().unwrap();
        let raw_a = seed_source_set(&conn_a, dir.path());
        let reg = register_master(&conn_a, raw_a, &master, "{}").unwrap();

        // Path B: scanner ingestion (fresh DB, scan the directory containing
        // ONLY the master file — copy it to an isolated dir first).
        let scan_dir = tempfile::tempdir().unwrap();
        std::fs::copy(&master, scan_dir.path().join("master_dark.fits")).unwrap();
        let conn_b = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn_b).unwrap();
        conn_b.execute("INSERT INTO scan_roots (path) VALUES (?1)",
            [scan_dir.path().to_string_lossy()]).unwrap();
        let scan_result = crate::scanner::scan_directory(
            scan_dir.path(), &conn_b, None, false, false, 1,
        );
        assert!(scan_result.errors.is_empty(), "scan errors: {:?}", scan_result.errors);

        const FRAME_COLS: &str =
            "imagetyp, is_master, instrume, exptime, gain, offset, binning, ccd_temp";
        let row = |conn: &Connection, where_: &str| -> Vec<Option<String>> {
            conn.query_row(
                &format!("SELECT {FRAME_COLS} FROM frames WHERE {where_}"),
                [],
                |r| Ok((0..8).map(|i| col_to_string(r.get_ref(i).unwrap())).collect()),
            ).unwrap()
        };
        let a = row(&conn_a, &format!("id = {}", reg.master_frame_id));
        let b = row(&conn_b, "is_master = 1");
        assert_eq!(a, b, "direct-registration frame row must equal scanner-ingested frame row");

        const SET_COLS: &str =
            "imagetyp, is_master_library, frame_count, exptime, gain, offset, binning, instrume";
        let set_row = |conn: &Connection, where_: &str| -> Vec<Option<String>> {
            conn.query_row(
                &format!("SELECT {SET_COLS} FROM calibration_set WHERE {where_}"),
                [],
                |r| Ok((0..8).map(|i| col_to_string(r.get_ref(i).unwrap())).collect()),
            ).unwrap()
        };
        let set_a = set_row(&conn_a, &format!("id = {}", reg.master_set_id));
        let set_b = set_row(&conn_b, "is_master_library = 1");
        assert_eq!(set_a, set_b, "direct-registration set row must equal scanner-ingested set row");
    }

    #[test]
    fn matcher_excludes_superseded_sets() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        let raw = seed_source_set(&conn, dir.path());
        let master_path = write_master(dir.path());
        let reg = register_master(&conn, raw, &master_path, "{}").unwrap();

        // Parse a real frame to drive the matcher.
        let probe = dir.path().join("probe.fits");
        let cards = crate::fits_writer::keywords::HeaderBuilder::new(FrameKind::Light)
            .instrume("TestCam").exptime(300.0).gain(100).offset(50)
            .binning(1, 1).ccd_temp(-10.0)
            .build().unwrap();
        crate::fits_writer::write_fits_f32(&probe, 8, 8, 1, &vec![1.0; 64], &cards).unwrap();
        let frame = crate::fits_parser::parse_fits(&probe, 0).unwrap();
        let config = crate::calibration::config::CalibrationMatchingConfig::default();
        let candidates = crate::calibration::configurable_matcher::find_calibration_candidates(
            &conn, &frame, "lights", "dark", &config,
            crate::calibration::finder::CandidateMode::IncludeIncompatible,
        ).unwrap();
        assert!(candidates.iter().all(|c| c.set_id != raw),
            "superseded raw set must never appear as a candidate");
        assert!(candidates.iter().any(|c| c.set_id == reg.master_set_id),
            "the master must appear instead");
    }
}
