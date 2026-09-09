//! Catalog test fixtures shared by the stacking pipeline's own tests (Plan
//! 5a Task 3 onward — `groups`, and later `paths`/`plan`/`run`/`provenance`
//! tests): a frame set with one imaging night and one session, LIGHT frames
//! (optionally a real 16-bit FITS on disk with a synthetic star field), and
//! a built master dark + master flat linked to a given set of lights.
//!
//! Every row shape here mirrors the equivalent seed helper in
//! `api::lights.rs`'s test module (`seed_frame_set`, `seed_light`,
//! `seed_set`, `add_link`, `seed_master_with_file`) — same NOT NULL columns,
//! same junction-table shape — so a query written against one fixture keeps
//! working against the other.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use crate::db::schema::init_db;
use crate::fits_writer::card::{format_card, Card, CardValue, BLOCK_SIZE, CARD_SIZE};
use crate::fits_writer::write_fits_f32;
use crate::test_support::gaussian_field;

/// One fixture's whole catalog: an in-memory `Connection` seeded with a
/// frame set, one imaging night spanning it, and one session inside that
/// night — every LIGHT `add_light` adds joins into `session_id` the same way
/// the scanner's own session detection would. `dir` is the fixture's private
/// scratch directory (FITS files, and a subdirectory a caller can use as a
/// generation scratch dir); it is dropped (and removed from disk) with the
/// `Fixture`.
pub(crate) struct Fixture {
    pub conn: Connection,
    pub dir: tempfile::TempDir,
    pub set_id: i64,
    // Not read by this task's own tests, but part of the interface — a later
    // task (e.g. multi-night grouping) may need to seed a second night and
    // will want this one's id to keep the first session anchored to it.
    #[allow(dead_code)]
    pub night_id: i64,
    pub session_id: i64,
}

/// A frame set + one imaging night + one session, ready for `add_light`.
pub(crate) fn frame_set(name: &str) -> Fixture {
    let conn = Connection::open_in_memory().expect("open in-memory fixture catalog");
    init_db(&conn).expect("init fixture schema");

    conn.execute("INSERT INTO frames_set (name) VALUES (?1)", params![name])
        .unwrap();
    let set_id = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
         VALUES (?1, '2025-01-01T00:00:00Z', '2025-01-02T00:00:00Z')",
        params![set_id],
    )
    .unwrap();
    let night_id = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'fixture')",
        params![night_id],
    )
    .unwrap();
    let session_id = conn.last_insert_rowid();

    let dir = tempfile::tempdir().expect("fixture scratch dir");
    Fixture {
        conn,
        dir,
        set_id,
        night_id,
        session_id,
    }
}

/// One LIGHT frame to seed via [`add_light`].
pub(crate) struct LightSpec<'a> {
    pub stem: &'a str,
    pub instrume: &'a str,
    pub filter: Option<&'a str>,
    pub binning: i64,
    pub width: usize,
    pub height: usize,
    pub exptime: f64,
    pub date_obs: &'a str,
    pub bayerpat: Option<&'a str>,
    /// When true, write a real 16-bit FITS (`BITPIX 16`, `BZERO 32768`) of
    /// `width x height` with a synthetic star field to `dir`; when false,
    /// only the catalog rows are seeded (the `files.path` still points
    /// somewhere under `dir`, just with nothing written there) — cheaper for
    /// tests that only exercise the catalog-side grouping/selection logic.
    pub write_file: bool,
}

/// One LIGHT frame (`files` + `frames` rows) joined into the fixture's
/// session, mirroring `api::lights.rs`'s `seed_light` plus the columns
/// integration groups need (`instrume`, `filter`, `xbinning`, `naxis1/2`,
/// `exptime`, `date_obs`, `bayerpat`). Returns `(frame_id, path)`.
pub(crate) fn add_light(f: &Fixture, spec: &LightSpec<'_>) -> (i64, PathBuf) {
    let filename = format!("{}.fits", spec.stem);
    let path = f.dir.path().join(&filename);

    // Honest identity when a file actually exists on disk (Task 6's artifact
    // hashes and the catalog-vs-disk `disk_matches_row` contract read these
    // columns); the pre-existing placeholder (`size = 0`, `modified_at` = the
    // spec's own `date_obs`) otherwise, since there is nothing on disk to
    // stat.
    let (size, modified_at) = if spec.write_file {
        write_light_fits(&path, spec);
        file_identity(&path)
    } else {
        (0, spec.date_obs.to_string())
    };

    f.conn
        .execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, ?4, 'FITS')",
            params![path.to_string_lossy(), filename, size, modified_at],
        )
        .unwrap();
    let file_id = f.conn.last_insert_rowid();

    f.conn
        .execute(
            "INSERT INTO frames
                (file_id, instrume, filter, xbinning, naxis1, naxis2, exptime, date_obs, bayerpat, imagetyp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'Light')",
            params![
                file_id,
                spec.instrume,
                spec.filter,
                spec.binning,
                spec.width as i64,
                spec.height as i64,
                spec.exptime,
                spec.date_obs,
                spec.bayerpat,
            ],
        )
        .unwrap();
    let frame_id = f.conn.last_insert_rowid();

    f.conn
        .execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            params![f.session_id, frame_id],
        )
        .unwrap();

    (frame_id, path)
}

/// A built master dark (constant 100 ADU) + a built master flat (constant
/// 0.5, `ATH_FNRM = 0.5`) — both float32 FITS on disk, both registered as
/// `calibration_set` rows with `is_master_library = 1` (the row shape
/// `api::lights.rs`'s `seed_master_with_file` uses), and linked ("Dark" /
/// "Flat") to every frame id in `light_frame_ids`. Returns `(dark_set_id,
/// flat_set_id)`.
pub(crate) fn add_master_dark_and_flat(
    f: &Fixture,
    light_frame_ids: &[i64],
    width: usize,
    height: usize,
) -> (i64, i64) {
    let dark_set = seed_master(f, "MasterDark", width, height, 100.0, None);
    let flat_set = seed_master(f, "MasterFlat", width, height, 0.5, Some(0.5));
    for &frame_id in light_frame_ids {
        link_calibration(&f.conn, frame_id, dark_set, "Dark");
        link_calibration(&f.conn, frame_id, flat_set, "Flat");
    }
    (dark_set, flat_set)
}

/// One built master: a `calibration_set` row (`is_master_library = 1`,
/// `frame_count = 1`) plus a `files`/`frames` row pointing at a real
/// constant-value float32 FITS on disk (`ATH_FNRM` stamped when given, the
/// way the calibration library normalizes a master flat).
fn seed_master(
    f: &Fixture,
    imagetyp: &str,
    width: usize,
    height: usize,
    value: f32,
    ath_fnrm: Option<f64>,
) -> i64 {
    f.conn
        .execute(
            "INSERT INTO calibration_set (imagetyp, date, is_master_library, frame_count)
             VALUES (?1, '2025-01-01', 1, 1)",
            params![imagetyp],
        )
        .unwrap();
    let set_id = f.conn.last_insert_rowid();

    let filename = format!("{}_{set_id}.fits", imagetyp.to_ascii_lowercase());
    let path = f.dir.path().join(&filename);
    let data = vec![value; width * height];
    let mut cards = vec![Card::new("IMAGETYP", CardValue::Str(imagetyp.to_string())).unwrap()];
    if let Some(n) = ath_fnrm {
        cards.push(Card::new("ATH_FNRM", CardValue::Real(n)).unwrap());
    }
    write_fits_f32(&path, width, height, 1, &data, &cards).expect("write fixture master FITS");
    let (size, modified_at) = file_identity(&path);

    f.conn
        .execute(
            "INSERT INTO files (path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, ?4, 'FITS')",
            params![path.to_string_lossy(), filename, size, modified_at],
        )
        .unwrap();
    let file_id = f.conn.last_insert_rowid();

    f.conn
        .execute(
            "INSERT INTO frames (file_id, imagetyp, is_master) VALUES (?1, ?2, 1)",
            params![file_id, imagetyp],
        )
        .unwrap();
    let frame_id = f.conn.last_insert_rowid();

    f.conn
        .execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            params![set_id, frame_id],
        )
        .unwrap();

    set_id
}

/// Real on-disk `(size, modified_at)` for a just-written file, in the same
/// shape the scanner stores: `size` = byte length, `modified_at` =
/// `chrono::DateTime::<Utc>::from(metadata.modified()).to_rfc3339()` — the
/// scanner's own `modified_dt.to_rfc3339()` (`scanner/mod.rs`, `modified_dt
/// = chrono::DateTime::<Utc>::from(metadata.modified()?)`). Task 6's
/// artifact hashes and the catalog-vs-disk `disk_matches_row` contract
/// (`db::bank_strong_hash`) both compare against these two columns, so a
/// fixture that fakes them would pass generation today and silently stop
/// meaning anything the moment either of those checks the fixture.
fn file_identity(path: &Path) -> (i64, String) {
    let meta = std::fs::metadata(path).expect("fixture file exists after writing");
    let modified = meta.modified().expect("fixture filesystem supports mtime");
    let modified_at = chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339();
    (meta.len() as i64, modified_at)
}

fn link_calibration(conn: &Connection, frame_id: i64, set_id: i64, cal_type: &str) {
    conn.execute(
        "INSERT INTO calibration_set_to_frames
            (source_id, source_type, calibration_set_id, calibration_type, matched_at)
         VALUES (?1, 'frame', ?2, ?3, '2025-01-01T00:00:00Z')",
        params![frame_id, set_id, cal_type],
    )
    .unwrap();
}

// ── Real 16-bit FITS for a LIGHT fixture ────────────────────────────────────

/// Write a real 16-bit (`BITPIX 16`, `BZERO 32768`, `BSCALE 1`) FITS with a
/// synthetic star field (`test_support::gaussian_field`, background 500 ADU,
/// three stars between ~2500 and ~14000 ADU) and the header cards the
/// pipeline reads: `INSTRUME`, `FILTER` (when given), `XBINNING`, `EXPTIME`,
/// `DATE-OBS`, `IMAGETYP`, `BAYERPAT` (when given), `ROWORDER`.
fn write_light_fits(path: &Path, spec: &LightSpec<'_>) {
    let (w, h) = (spec.width, spec.height);
    let stars: Vec<(f64, f64, f64)> = vec![
        (w as f64 * 0.3, h as f64 * 0.35, 6000.0),
        (w as f64 * 0.62, h as f64 * 0.55, 14000.0),
        (w as f64 * 0.45, h as f64 * 0.72, 2500.0),
    ];
    let plane = gaussian_field(w, h, &stars, 1.6, 500.0);
    // Encode as unsigned 16-bit via the standard BZERO=32768 offset: stored
    // signed value = physical value - 32768.
    let data: Vec<i16> = plane
        .iter()
        .map(|&v| (v.round().clamp(0.0, 65535.0) as i32 - 32768) as i16)
        .collect();

    let mut cards = vec![Card::new("INSTRUME", CardValue::Str(spec.instrume.to_string())).unwrap()];
    if let Some(filter) = spec.filter {
        cards.push(Card::new("FILTER", CardValue::Str(filter.to_string())).unwrap());
    }
    cards.push(Card::new("XBINNING", CardValue::Integer(spec.binning)).unwrap());
    cards.push(Card::new("EXPTIME", CardValue::Real(spec.exptime)).unwrap());
    cards.push(Card::new("DATE-OBS", CardValue::Str(spec.date_obs.to_string())).unwrap());
    cards.push(Card::new("IMAGETYP", CardValue::Str("Light".to_string())).unwrap());
    if let Some(bayerpat) = spec.bayerpat {
        cards.push(Card::new("BAYERPAT", CardValue::Str(bayerpat.to_string())).unwrap());
    }
    cards.push(Card::new("ROWORDER", CardValue::Str("TOP-DOWN".to_string())).unwrap());

    write_fits_i16(path, w, h, &data, &cards);
}

/// Hand-rolled 16-bit FITS primary-HDU writer (there is no `write_fits_i16`
/// in `fits_writer` — only the float32 `write_fits_f32`): structural cards
/// (`SIMPLE`/`BITPIX`/`NAXIS`/`NAXIS1`/`NAXIS2`/`BZERO`/`BSCALE`) via
/// `Card::structural` the same way `fits_writer::writer` builds its own
/// header, then the caller's cards, `END`, header padded to a 2880-byte
/// block, then big-endian `i16` data padded to a 2880-byte block.
fn write_fits_i16(path: &Path, width: usize, height: usize, data: &[i16], cards: &[Card]) {
    fn push(records: &mut Vec<[u8; CARD_SIZE]>, c: Card) {
        records.extend(format_card(&c).expect("valid fixture header card"));
    }

    let mut records: Vec<[u8; CARD_SIZE]> = Vec::new();
    push(
        &mut records,
        Card::structural(
            "SIMPLE",
            CardValue::Logical(true),
            "conforms to FITS standard",
        ),
    );
    push(
        &mut records,
        Card::structural("BITPIX", CardValue::Integer(16), "16-bit signed integer"),
    );
    push(
        &mut records,
        Card::structural("NAXIS", CardValue::Integer(2), "number of data axes"),
    );
    push(
        &mut records,
        Card::structural("NAXIS1", CardValue::Integer(width as i64), "width"),
    );
    push(
        &mut records,
        Card::structural("NAXIS2", CardValue::Integer(height as i64), "height"),
    );
    push(
        &mut records,
        Card::structural(
            "BZERO",
            CardValue::Real(32768.0),
            "offset for unsigned 16-bit",
        ),
    );
    push(
        &mut records,
        Card::structural("BSCALE", CardValue::Real(1.0), "data scaling"),
    );
    for c in cards {
        records.extend(format_card(c).expect("valid fixture header card"));
    }
    let mut end = [b' '; CARD_SIZE];
    end[..3].copy_from_slice(b"END");
    records.push(end);

    let mut bytes = Vec::with_capacity(records.len() * CARD_SIZE + data.len() * 2 + BLOCK_SIZE);
    for r in &records {
        bytes.extend_from_slice(r);
    }
    let header_pad = (BLOCK_SIZE - bytes.len() % BLOCK_SIZE) % BLOCK_SIZE;
    bytes.extend(std::iter::repeat(b' ').take(header_pad));

    for v in data {
        bytes.extend_from_slice(&v.to_be_bytes());
    }
    let data_bytes = data.len() * 2;
    let data_pad = (BLOCK_SIZE - data_bytes % BLOCK_SIZE) % BLOCK_SIZE;
    bytes.extend(std::iter::repeat(0u8).take(data_pad));

    std::fs::write(path, &bytes).expect("write fixture light FITS");
}

// ── Fixture self-test ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{resolve_generation, CalibratedLightOptions};

    /// Task 6 reuses this fixture builder to run the real calibrated-export
    /// generator; this pins that the shape it seeds is already enough for
    /// `resolve_generation` (catalog phase — master resolution, source
    /// cards, CFA geometry, flat-norm divisor) to succeed, so a later task's
    /// failure means ITS code, not a fixture gap. Also pins the
    /// mono-vs-OSC debayer decision `resolve_generation` makes
    /// (`opts.debayer_osc && resolved.cfa_geometry.is_some()`, default
    /// options): a mono light must not debayer, an OSC light with a
    /// catalog-recognized Bayer pattern must — the second group Task 6/8
    /// actually runs the VNG path on.
    #[test]
    fn fixture_light_resolves_a_calibration_plan() {
        let f = frame_set("Fixture Self-Test");
        let (mono_id, _mono_path) = add_light(
            &f,
            &LightSpec {
                stem: "light_mono",
                instrume: "TestCam",
                filter: Some("L"),
                binning: 1,
                width: 32,
                height: 24,
                exptime: 60.0,
                date_obs: "2025-01-01T00:00:00",
                bayerpat: None,
                write_file: true,
            },
        );
        let (osc_id, _osc_path) = add_light(
            &f,
            &LightSpec {
                stem: "light_osc",
                instrume: "TestCam",
                filter: None,
                binning: 1,
                width: 32,
                height: 24,
                exptime: 60.0,
                date_obs: "2025-01-01T00:01:00",
                bayerpat: Some("RGGB"),
                write_file: true,
            },
        );
        add_master_dark_and_flat(&f, &[mono_id, osc_id], 32, 24);

        let scratch = f.dir.path().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();

        let mono_spec = resolve_generation(
            &f.conn,
            mono_id,
            &CalibratedLightOptions::default(),
            &scratch,
        )
        .expect("mono fixture light must resolve a calibration plan");
        assert!(
            mono_spec.inputs.dark_path.is_some(),
            "dark master must resolve"
        );
        assert!(
            mono_spec.inputs.flat_path.is_some(),
            "flat master must resolve"
        );
        assert!(!mono_spec.debayer, "mono light must not debayer");

        let osc_spec = resolve_generation(
            &f.conn,
            osc_id,
            &CalibratedLightOptions::default(),
            &scratch,
        )
        .expect("OSC fixture light must resolve a calibration plan");
        assert!(
            osc_spec.debayer,
            "OSC light with a resolvable Bayer pattern must debayer"
        );
    }
}
