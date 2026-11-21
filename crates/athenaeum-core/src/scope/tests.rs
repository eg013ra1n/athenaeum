//! Scope tests: the night rule against SQLite itself, and every condition against a small real catalog.

use super::night::{night_of_date_obs, night_sql};
use super::*;
use crate::db::{self, Database};
use crate::models::{File, FileFormat, Frame, ImageType};
use crate::test_support::canonical_tempdir;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

struct Cat {
    // Fields drop in declaration order: the pool holding `catalog.db` open goes before its folder is removed.
    db: Database,
    base: PathBuf,
    _tmp: tempfile::TempDir,
}

fn cat() -> Cat {
    let (tmp, base) = canonical_tempdir();
    let db = Database::new(base.join("catalog.db")).expect("open catalog");
    Cat { db, base, _tmp: tmp }
}

/// One frame to put in the catalog; `path` is relative to the catalog's folder.
#[derive(Default, Clone)]
struct Fx {
    path: &'static str,
    object: Option<&'static str>,
    camera: Option<&'static str>,
    telescope: Option<&'static str>,
    filter: Option<&'static str>,
    imagetyp: Option<ImageType>,
    exptime: Option<f64>,
    date_obs: Option<&'static str>,
    is_master: bool,
}

impl Cat {
    fn path(&self, rel: &str) -> String {
        rel.split('/').fold(self.base.clone(), |p, part| p.join(part)).to_string_lossy().into_owned()
    }

    /// Returns `(file id, frame id)`.
    fn add(&self, fx: &Fx) -> (i64, i64) {
        let conn = self.db.conn();
        let path = self.path(fx.path);
        let filename = Path::new(&path).file_name().unwrap().to_string_lossy().into_owned();
        let file_id = db::insert_file(
            &conn,
            &File {
                id: None,
                path,
                filename,
                size: 10,
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
        .expect("insert file");
        let frame_id = db::insert_frame(
            &conn,
            &Frame {
                file_id,
                object: fx.object.map(str::to_string),
                instrume: fx.camera.map(str::to_string),
                telescop: fx.telescope.map(str::to_string),
                filter: fx.filter.map(str::to_string),
                imagetyp: fx.imagetyp.clone(),
                exptime: fx.exptime,
                is_master: fx.is_master,
                date_obs: fx
                    .date_obs
                    .map(|d| DateTime::parse_from_rfc3339(d).expect("test date").with_timezone(&Utc)),
                ..Default::default()
            },
        )
        .expect("insert frame");
        (file_id, frame_id)
    }

    /// A frame set holding `frame_ids`, one night and session.
    fn set_of(&self, name: Option<&str>, archived: bool, frame_ids: &[i64]) -> i64 {
        let conn = self.db.conn();
        let set = db::create_frames_set(&conn, name, false, None, None, None, None, None, None, None, None)
            .expect("set");
        if archived {
            conn.execute("UPDATE frames_set SET is_archived = 1 WHERE id = ?1", [set]).expect("archive set");
        }
        let night = db::create_imaging_night(&conn, set, "2024-01-01T00:00:00Z", "2024-01-01T12:00:00Z")
            .expect("night");
        let session = db::create_session(&conn, night, "cam", frame_ids.len() as i32, None).expect("session");
        db::insert_session_members(&conn, session, frame_ids).expect("members");
        set
    }

    fn black_hole(&self, file_id: i64) {
        self.db
            .conn()
            .execute(
                "INSERT INTO black_hole (file_id, from_where, moved_at, original_path) VALUES (?1, 'test', 'now', 'x')",
                [file_id],
            )
            .expect("black hole");
    }

    fn in_zip(&self, file_id: i64) {
        self.db
            .conn()
            .execute("UPDATE files SET archive_zip_path = ?2 WHERE id = ?1", rusqlite::params![file_id, self.path("a.zip")])
            .expect("zip");
    }

    fn scan_root(&self, rel: &str) -> String {
        let path = self.path(rel);
        db::upsert_scan_root(&self.db.conn(), &path, "normal").expect("scan root");
        path
    }

    /// File names the filter lets through, sorted.
    fn names(&self, filter: &ScopeFilter) -> Vec<String> {
        let conn = self.db.conn();
        let sql = format!(
            "SELECT ff.filename FROM frames fr JOIN files ff ON ff.id = fr.file_id WHERE {} ORDER BY ff.filename",
            filter.sql
        );
        let mut stmt = conn.prepare(&sql).unwrap_or_else(|e| panic!("prepare {sql}: {e}"));
        let rows = stmt
            .query_map(rusqlite::params_from_iter(filter.params.iter()), |r| r.get::<_, String>(0))
            .unwrap_or_else(|e| panic!("run {sql}: {e}"));
        rows.map(|r| r.unwrap()).collect()
    }

    fn resolve(&self, scope: &Scope) -> ResolvedScope {
        resolve_scope(&self.db.conn(), scope).expect("resolve")
    }

    fn scoped_names(&self, scope: &Scope) -> Vec<String> {
        let r = self.resolve(scope);
        let f = r.filter.unwrap_or_else(|| panic!("no filter: {}", hints_text(&r.hints)));
        self.names(&f)
    }
}

fn light(path: &'static str) -> Fx {
    Fx { path, imagetyp: Some(ImageType::Light), ..Default::default() }
}

// ---------------------------------------------------------------------------------------------------------------
// The night rule
// ---------------------------------------------------------------------------------------------------------------

/// Scope (SQL) and grouping (Rust) must put every frame on the same night: one row per form, against SQLite.
#[test]
fn night_sql_equals_night_of_date_obs() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let sql = format!("SELECT {}", night_sql("?1"));
    let forms = [
        // The forms the scanner and the collab manifests write.
        "2026-09-27T11:59:59Z",
        "2026-09-27T12:00:00Z",
        "2026-09-27T03:10:00",
        "2026-09-27T03:10:00.250",
        "2026-09-27T23:30:00+02:00",
        "2026-09-27T03:10:00+00:00",
        "2024-10-05 22:15:03",
        "2024-10-05",
        "garbage",
        // Shapes around them.
        "2024-10-05T22:15",
        "2024-10-05T22:15Z",
        "2024-10-05T22:15:03z",
        "2024-10-05 22:15:03 +05:30",
        "2024-10-05T22:15:03-08:00",
        "2024-10-05T03:10:00-08:00",
        "2024-10-05T11:59:59.9996Z",
        "2024-10-05T23:59:59.9996Z",
        "2024-10-05T22:15:03.",
        "2024-10-05T22:15:03.123456789012",
        "2024-10-05T",
        "2024-10-05 ",
        "2024-10-05  T 22:15:03",
        "2024-02-31T20:00:00",
        "2024-02-30",
        "2023-03-01T01:00:00Z",
        "2024-01-01T05:00:00Z",
        "0000-01-01T05:00:00Z",
        "9999-12-31T23:59:59Z",
        "-0001-06-01T20:00:00Z",
        // Malformed: SQLite answers NULL for most of these and a date for a few (`T24:00:00`, a fraction before
        // `Z`, a trailing space); the Rust twin must answer the same either way.
        "",
        "2024-13-01",
        "2024-00-10",
        "2024-10-00",
        "2024-10-32",
        "2024-10-05T24:00:00",
        "2024-10-05T22:60:00",
        "2024-10-05T22:15:60",
        "2024-10-05T22:15:03+15:00",
        "2024-10-05T22:15:03+0200",
        "2024-10-05T22:15:03+02",
        "2024-10-05T22:15:03Zjunk",
        "2024-10-05T22:15:03 junk",
        "2024-10-05Tgarbage",
        "2024-1-5",
        "20241005",
        "2024-10-05T22",
        "2024-10-05T22:1",
        " 2024-10-05",
        "2024/10/05",
        "2024-10-05T22:15:03.5Z",
        "2024-10-05T22:15:03Z ",
    ];
    for form in forms {
        let sqlite: Option<String> = conn.query_row(&sql, [form], |r| r.get(0)).unwrap();
        assert_eq!(night_of_date_obs(form), sqlite, "night of {form:?}");
    }
}

#[test]
fn night_of_date_obs_shifts_twelve_hours_in_utc() {
    assert_eq!(night_of_date_obs("2026-09-27T11:59:59Z").as_deref(), Some("2026-09-26"));
    assert_eq!(night_of_date_obs("2026-09-27T12:00:00Z").as_deref(), Some("2026-09-27"));
    assert_eq!(night_of_date_obs("2026-09-27T03:10:00").as_deref(), Some("2026-09-26"));
    assert_eq!(night_of_date_obs("2026-09-27T03:10:00.250").as_deref(), Some("2026-09-26"));
    assert_eq!(night_of_date_obs("garbage"), None);
}

// ---------------------------------------------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------------------------------------------

fn everything_scope(c: &Cat, scan_root: &str) -> Scope {
    Scope {
        object: Some("m 31".into()),
        frame_set: Some("andromeda".into()),
        folder: Some(c.path("M31")),
        scan_root: Some(scan_root.to_string()),
        camera: Some("ZWO ASI2600MM Pro".into()),
        telescope: Some("RC8".into()),
        filter: Some("Ha".into()),
        frame_type: Some("lights".into()),
        exposure_s: Some(300.0),
        night: Some("2024-10-04".into()),
        date_from: Some("2024-01-01".into()),
        date_to: Some("2024-12-31".into()),
        year: Some(2024),
        state: Some(vec![FrameState::Catalog, FrameState::SoftArchived, FrameState::Zipped, FrameState::BlackHole]),
        tab: None,
        files: None,
    }
}

/// Every statement the resolver builds prepares and runs under the `f` = frames alias, on an empty and on a
/// populated catalog (a wrong alias shows as "no such column").
#[test]
fn every_scope_statement_prepares_and_runs() {
    // Empty catalog: the conditions that need no catalog value.
    let c = cat();
    let scope = Scope {
        folder: Some(c.path("M31")),
        frame_type: Some("lights".into()),
        exposure_s: Some(300.0),
        night: Some("2024-10-04".into()),
        date_from: Some("2024-01-01".into()),
        date_to: Some("2024-12-31".into()),
        year: Some(2024),
        ..Default::default()
    };
    assert_eq!(c.scoped_names(&scope), Vec::<String>::new());
    for state in FrameState::ALL {
        let f = ScopeFilter { sql: format!("fr.id IN (SELECT f.id FROM frames f JOIN files fl ON fl.id = f.file_id WHERE {})", frame_state_sql(state)), params: vec![] };
        assert_eq!(c.names(&f), Vec::<String>::new());
    }
    // An empty catalog names nothing: every text value is a hint, none an error.
    let r = c.resolve(&Scope { camera: Some("x".into()), object: Some("x".into()), frame_set: Some("x".into()), ..Default::default() });
    assert!(r.filter.is_none());
    assert_eq!(r.hints.len(), 3);

    // Populated: all conditions at once.
    let root = c.scan_root("M31");
    let (_, fr1) = c.add(&Fx {
        path: "M31/H/a.fits",
        object: Some("M 31"),
        camera: Some("ZWO ASI2600MM Pro"),
        telescope: Some("RC8"),
        filter: Some("Ha"),
        imagetyp: Some(ImageType::Light),
        exptime: Some(300.0),
        date_obs: Some("2024-10-05T02:00:00+00:00"),
        ..Default::default()
    });
    c.set_of(Some("Andromeda"), false, &[fr1]);
    let s = everything_scope(&c, &root);
    assert_eq!(c.scoped_names(&s), vec!["a.fits".to_string()]);
    // And the same scope with the default states.
    let s = Scope { state: None, ..s };
    assert_eq!(c.scoped_names(&s), vec!["a.fits".to_string()]);
}

#[test]
fn camera_matches_normalized_not_substring() {
    let c = cat();
    c.add(&Fx { path: "a.fits", camera: Some("ZWO ASI2600MM Pro"), ..light("a.fits") });
    c.add(&Fx { path: "b.fits", camera: Some("ZWO ASI2600MC Duo"), ..light("b.fits") });
    let r = c.resolve(&Scope { camera: Some("zwo  asi2600mm pro".into()), ..Default::default() });
    assert_eq!(c.names(r.filter.as_ref().expect("resolves")), vec!["a.fits".to_string()]);
    let r = c.resolve(&Scope { camera: Some("ASI2600".into()), ..Default::default() });
    assert!(r.filter.is_none());
    assert_eq!(
        hints_text(&r.hints),
        "no camera 'ASI2600'; the catalog has 'ZWO ASI2600MC Duo' and 'ZWO ASI2600MM Pro'"
    );
}

#[test]
fn describe_reads_like_the_spec() {
    let s = Scope {
        camera: Some("ZWO ASI2600MM Pro".into()),
        year: Some(2024),
        frame_type: Some("lights".into()),
        ..Default::default()
    };
    assert_eq!(describe_scope(&s), "ZWO ASI2600MM Pro, nights 2024-01-01 … 2024-12-31, lights");
    assert_eq!(describe_scope(&Scope::default()), "the whole catalog");
    let s = Scope {
        object: Some("IC 5146".into()),
        exposure_s: Some(300.0),
        frame_type: Some("flat".into()),
        folder: Some("/data/IC5146".into()),
        state: Some(vec![FrameState::Catalog, FrameState::SoftArchived, FrameState::Zipped, FrameState::BlackHole]),
        ..Default::default()
    };
    assert_eq!(describe_scope(&s), "IC 5146, 300 s, flats, in /data/IC5146, including the Black Hole");
    for (given, said) in [
        ("bias", "biases"),
        ("biases", "biases"),
        ("master bias", "master biases"),
        ("master darks", "master darks"),
        ("darkflat", "darkflats"),
    ] {
        let s = Scope { frame_type: Some(given.into()), ..Default::default() };
        assert_eq!(describe_scope(&s), said, "frame type {given:?}");
    }
}

#[test]
fn black_hole_left_out_unless_asked() {
    let c = cat();
    c.add(&light("keep.fits"));
    let (bh, _) = c.add(&light("rejected.fits"));
    c.black_hole(bh);
    let r = c.resolve(&Scope::default());
    assert!(r.black_hole_left_out);
    assert_eq!(c.names(r.filter.as_ref().unwrap()), vec!["keep.fits".to_string()]);

    let ask = Scope { state: Some(vec![FrameState::Catalog, FrameState::BlackHole]), ..Default::default() };
    let r = c.resolve(&ask);
    assert!(!r.black_hole_left_out);
    assert_eq!(c.names(r.filter.as_ref().unwrap()), vec!["keep.fits".to_string(), "rejected.fits".to_string()]);

    let only = Scope { state: Some(vec![FrameState::BlackHole]), ..Default::default() };
    assert_eq!(c.scoped_names(&only), vec!["rejected.fits".to_string()]);
}

#[test]
fn folder_includes_subfolders_separator_strict() {
    let c = cat();
    c.add(&light("M31/H/a.fits"));
    c.add(&light("M31b/a.fits"));
    let r = c.scoped_names(&Scope { folder: Some(c.path("M31")), ..Default::default() });
    assert_eq!(r, vec!["a.fits".to_string()]);
    // The second `a.fits` is the sibling: counting rows says which one came back.
    let both = c.scoped_names(&Scope { folder: Some(c.path("M31b")), ..Default::default() });
    assert_eq!(both, vec!["a.fits".to_string()]);
    let all = c.scoped_names(&Scope::default());
    assert_eq!(all.len(), 2);
    // A trailing separator names the same folder.
    let slash = format!("{}{}", c.path("M31"), std::path::MAIN_SEPARATOR);
    assert_eq!(c.scoped_names(&Scope { folder: Some(slash), ..Default::default() }), vec!["a.fits".to_string()]);
}

#[test]
fn scan_root_is_matched_by_its_path_or_hinted() {
    let c = cat();
    let root = c.scan_root("M31");
    c.scan_root("Other");
    c.add(&light("M31/H/a.fits"));
    c.add(&light("Other/b.fits"));
    assert_eq!(
        c.scoped_names(&Scope { scan_root: Some(root.clone()), ..Default::default() }),
        vec!["a.fits".to_string()]
    );
    let r = c.resolve(&Scope { scan_root: Some(c.path("M3")), ..Default::default() });
    assert!(r.filter.is_none());
    assert_eq!(r.hints[0].condition, "scan root");
    assert_eq!(r.hints[0].existing.len(), 1);
}

#[test]
fn object_matches_frame_set_name() {
    let c = cat();
    let (_, in_set) = c.add(&Fx { path: "in_set.fits", object: None, ..light("in_set.fits") });
    let (_, by_object) = c.add(&Fx { path: "by_object.fits", object: Some("IC 5146"), ..light("by_object.fits") });
    let (_, elsewhere) = c.add(&Fx { path: "elsewhere.fits", object: Some("M 31"), ..light("elsewhere.fits") });
    c.set_of(Some("IC 5146"), false, &[in_set]);
    c.set_of(Some("Andromeda"), false, &[elsewhere]);
    let _ = by_object;
    let names = c.scoped_names(&Scope { object: Some("ic 5146".into()), ..Default::default() });
    assert_eq!(names, vec!["by_object.fits".to_string(), "in_set.fits".to_string()]);
    // `frame_set` is the set only: the frame whose own OBJECT says the same is not in it.
    let names = c.scoped_names(&Scope { frame_set: Some("IC 5146".into()), ..Default::default() });
    assert_eq!(names, vec!["in_set.fits".to_string()]);
    // A name that is neither is a hint over both spellings.
    let r = c.resolve(&Scope { object: Some("IC 514".into()), ..Default::default() });
    assert!(r.filter.is_none());
    assert_eq!(r.hints[0].existing, vec!["IC 5146".to_string()]);
}

#[test]
fn year_is_the_night_range() {
    let c = cat();
    c.add(&Fx { path: "in2024.fits", date_obs: Some("2025-01-01T05:00:00+00:00"), ..light("in2024.fits") });
    c.add(&Fx { path: "in2025.fits", date_obs: Some("2025-01-01T12:00:00+00:00"), ..light("in2025.fits") });
    c.add(&Fx { path: "nodate.fits", ..light("nodate.fits") });
    let y2024 = c.scoped_names(&Scope { year: Some(2024), ..Default::default() });
    assert_eq!(y2024, vec!["in2024.fits".to_string()]);
    let y2025 = c.scoped_names(&Scope { year: Some(2025), ..Default::default() });
    assert_eq!(y2025, vec!["in2025.fits".to_string()]);
    let night = c.scoped_names(&Scope { night: Some("2024-12-31".into()), ..Default::default() });
    assert_eq!(night, vec!["in2024.fits".to_string()]);
    // Open-ended ranges.
    let from = c.scoped_names(&Scope { date_from: Some("2025-01-01".into()), ..Default::default() });
    assert_eq!(from, vec!["in2025.fits".to_string()]);
    let to = c.scoped_names(&Scope { date_to: Some("2024-12-31".into()), ..Default::default() });
    assert_eq!(to, vec!["in2024.fits".to_string()]);
}

#[test]
fn frame_type_and_exposure() {
    let c = cat();
    c.add(&Fx { path: "l.fits", exptime: Some(300.0), ..light("l.fits") });
    c.add(&Fx { path: "d.fits", exptime: Some(300.002), imagetyp: Some(ImageType::Dark), ..Default::default() });
    c.add(&Fx { path: "df.fits", exptime: Some(5.0), imagetyp: Some(ImageType::DarkFlat), ..Default::default() });
    let names = |s: Scope| c.scoped_names(&s);
    assert_eq!(names(Scope { frame_type: Some("lights".into()), ..Default::default() }), vec!["l.fits".to_string()]);
    assert_eq!(names(Scope { frame_type: Some("Dark".into()), ..Default::default() }), vec!["d.fits".to_string()]);
    assert_eq!(names(Scope { frame_type: Some("darkflats".into()), ..Default::default() }), vec!["df.fits".to_string()]);
    assert_eq!(
        names(Scope { exposure_s: Some(300.0), ..Default::default() }),
        vec!["d.fits".to_string(), "l.fits".to_string()]
    );
    let r = c.resolve(&Scope { frame_type: Some("ligths".into()), ..Default::default() });
    assert!(r.filter.is_none());
    assert_eq!(r.hints[0].condition, "frame type");
    assert_eq!(r.hints[0].existing, vec!["Light".to_string()]);
}

/// Each fixture frame is in exactly one state.
#[test]
fn states_partition_the_catalog() {
    let c = cat();
    let (_, plain) = c.add(&light("plain.fits"));
    let (_, in_live_set) = c.add(&light("live.fits"));
    let (_, in_archived_set) = c.add(&light("archived.fits"));
    let (zip_file, _) = c.add(&light("zipped.fits"));
    let (bh_file, _) = c.add(&light("blackholed.fits"));
    let (bh_zip_file, _) = c.add(&light("blackholed_zip.fits"));
    let _ = plain;
    c.set_of(Some("Live"), false, &[in_live_set]);
    c.set_of(Some("Archived"), true, &[in_archived_set]);
    c.in_zip(zip_file);
    c.black_hole(bh_file);
    c.in_zip(bh_zip_file);
    c.black_hole(bh_zip_file);

    let all = {
        let sql = "fr.id IN (SELECT f.id FROM frames f JOIN files fl ON fl.id = f.file_id WHERE 1)".to_string();
        c.names(&ScopeFilter { sql, params: vec![] })
    };
    assert_eq!(all.len(), 6);
    let by = |s: FrameState| {
        let sql = format!(
            "fr.id IN (SELECT f.id FROM frames f JOIN files fl ON fl.id = f.file_id WHERE {})",
            frame_state_sql(s)
        );
        c.names(&ScopeFilter { sql, params: vec![] })
    };
    assert_eq!(by(FrameState::Catalog), vec!["live.fits".to_string(), "plain.fits".to_string()]);
    assert_eq!(by(FrameState::SoftArchived), vec!["archived.fits".to_string()]);
    assert_eq!(by(FrameState::Zipped), vec!["zipped.fits".to_string()]);
    assert_eq!(by(FrameState::BlackHole), vec!["blackholed.fits".to_string(), "blackholed_zip.fits".to_string()]);
    let mut union: Vec<String> = FrameState::ALL.into_iter().flat_map(by).collect();
    union.sort();
    assert_eq!(union, all, "disjoint and exhaustive");
}

#[test]
fn hint_formats_and_nearest() {
    let h = |existing: &[&str]| NearestHint {
        condition: "filter",
        given: "Hb".into(),
        existing: existing.iter().map(|s| s.to_string()).collect(),
        looked_up: true,
    };
    assert_eq!(hints_text(&[h(&[])]), "no filter 'Hb'; nothing like it in the catalog");
    assert_eq!(hints_text(&[h(&["Ha"])]), "no filter 'Hb'; the catalog has 'Ha'");
    assert_eq!(hints_text(&[h(&["Ha", "Oiii", "Sii"])]), "no filter 'Hb'; the catalog has 'Ha', 'Oiii' and 'Sii'");
    let existing: Vec<String> = ["Ha", "OIII", "SII", "Luminance", "Red"].iter().map(|s| s.to_string()).collect();
    // Within three edits, by distance then value; never more than five.
    assert_eq!(nearest::nearest(&existing, "Hb"), vec!["Ha".to_string(), "Red".to_string(), "SII".to_string()]);
    let many: Vec<String> = (0..9).map(|i| format!("Filter {i}")).collect();
    assert_eq!(nearest::nearest(&many, "filter").len(), 5);
    assert_eq!(normalize_value("  ZWO   Asi \t2600 "), "zwo asi 2600");
}

/// Scope reaches the tool layer as JSON: camelCase fields, snake_case states.
#[test]
fn scope_json_shape() {
    let s: Scope = serde_json::from_str(
        r#"{"frameSet":"IC 5146","exposureS":300,"dateFrom":"2024-01-01","state":["soft_archived","black_hole"]}"#,
    )
    .unwrap();
    assert_eq!(s.frame_set.as_deref(), Some("IC 5146"));
    assert_eq!(s.exposure_s, Some(300.0));
    assert_eq!(s.state, Some(vec![FrameState::SoftArchived, FrameState::BlackHole]));
    assert_eq!(serde_json::to_string(&FrameState::BlackHole).unwrap(), "\"black_hole\"");
    let schema = serde_json::to_value(rmcp::schemars::schema_for!(Scope)).unwrap();
    assert!(schema["properties"]["frameSet"].is_object(), "{schema}");
}

/// The catalog's own flag decides raw or master: the scanner stores a master found by its file name as
/// `imagetyp = Dark`, `is_master = 1`.
#[test]
fn master_flag_decides_raw_or_master() {
    let c = cat();
    c.add(&Fx { path: "master_dark_300s.fits", imagetyp: Some(ImageType::Dark), is_master: true, ..Default::default() });
    c.add(&Fx { path: "plain_dark.fits", imagetyp: Some(ImageType::Dark), ..Default::default() });
    c.add(&Fx { path: "typed_master.fits", imagetyp: Some(ImageType::MasterDark), is_master: true, ..Default::default() });
    c.add(&light("light.fits"));
    let names = |t: &str| c.scoped_names(&Scope { frame_type: Some(t.into()), ..Default::default() });
    assert_eq!(names("darks"), vec!["plain_dark.fits".to_string()]);
    assert_eq!(names("dark"), vec!["plain_dark.fits".to_string()]);
    assert_eq!(names("master darks"), vec!["master_dark_300s.fits".to_string(), "typed_master.fits".to_string()]);
    assert_eq!(names("master dark"), vec!["master_dark_300s.fits".to_string(), "typed_master.fits".to_string()]);
    assert_eq!(names("lights"), vec!["light.fits".to_string()]);
}

/// A frame set's page counts its used copy only; a second copy of the same raw frame is `catalog` even when the
/// set is archived, and the four states still partition the catalog.
#[test]
fn soft_archived_counts_the_used_copy_only() {
    let c = cat();
    let (file_a, frame_a) = c.add(&light("A/dup.fits"));
    let (file_b, frame_b) = c.add(&light("B/dup.fits"));
    for file in [file_a, file_b] {
        c.db
            .conn()
            .execute(
                "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, 'SIMPLE = T', 'fp-dup')",
                [file],
            )
            .expect("header");
    }
    c.set_of(Some("Archived"), true, &[frame_a, frame_b]);
    let in_state = |folder: &str, state: FrameState| {
        c.scoped_names(&Scope { folder: Some(c.path(folder)), state: Some(vec![state]), ..Default::default() }).len()
    };
    // The lowest file id is the used copy.
    assert_eq!(in_state("A", FrameState::SoftArchived), 1);
    assert_eq!(in_state("A", FrameState::Catalog), 0);
    assert_eq!(in_state("B", FrameState::SoftArchived), 0);
    assert_eq!(in_state("B", FrameState::Catalog), 1);
    for folder in ["A", "B"] {
        let total: usize = FrameState::ALL.into_iter().map(|s| in_state(folder, s)).sum();
        assert_eq!(total, 1, "{folder} is in exactly one state");
    }
}

/// A scan root is matched by equality after separator normalisation: a spelling that differs in case is a miss
/// with the nearest roots as the hint.
#[test]
fn scan_root_case_difference_is_a_miss_with_a_hint() {
    let c = cat();
    let root = c.scan_root("Lights");
    c.add(&light("Lights/a.fits"));
    let wrong_case = c.path("lights");
    assert_ne!(wrong_case, root);
    let r = c.resolve(&Scope { scan_root: Some(wrong_case), ..Default::default() });
    assert!(r.filter.is_none());
    assert_eq!(r.hints[0].condition, "scan root");
    assert_eq!(r.hints[0].existing, vec![root.clone()]);
    // A trailing separator is still the same root.
    let slash = format!("{root}{}", std::path::MAIN_SEPARATOR);
    assert_eq!(c.scoped_names(&Scope { scan_root: Some(slash), ..Default::default() }), vec!["a.fits".to_string()]);
}

#[test]
fn malformed_night_is_an_error_in_words() {
    let c = cat();
    let err = |s: Scope| resolve_scope(&c.db.conn(), &s).expect_err("refused").to_string();
    assert_eq!(
        err(Scope { night: Some("last tuesday".into()), ..Default::default() }),
        "night must be a date YYYY-MM-DD, got 'last tuesday'"
    );
    assert_eq!(
        err(Scope { night: Some("2024-02-30".into()), ..Default::default() }),
        "night must be a date YYYY-MM-DD, got '2024-02-30'"
    );
    assert_eq!(
        err(Scope { date_from: Some("2024-1-5".into()), ..Default::default() }),
        "date from must be a date YYYY-MM-DD, got '2024-1-5'"
    );
    assert_eq!(
        err(Scope { date_to: Some("20241005".into()), ..Default::default() }),
        "date to must be a date YYYY-MM-DD, got '20241005'"
    );
    // A valid calendar date resolves, leap day included.
    assert!(resolve_scope(&c.db.conn(), &Scope { night: Some("2024-02-29".into()), ..Default::default() }).is_ok());
}

/// A misspelled field never quietly widens a scope.
#[test]
fn unknown_scope_field_is_refused() {
    let err = serde_json::from_str::<Scope>(r#"{"exposure": 300}"#).expect_err("refused").to_string();
    assert!(err.contains("exposure"), "{err}");
}

// ---------------------------------------------------------------------------------------------------------------
// Exact files, a frame-set handle and a tab
// ---------------------------------------------------------------------------------------------------------------

fn files_scope(entries: &[String]) -> Scope {
    Scope { files: Some(entries.to_vec()), ..Default::default() }
}

impl Cat {
    /// The line an answer prints for a file, read through the same query an answer uses.
    fn printed(&self, file_id: i64) -> crate::catalog_query::FileLine {
        let filter = ScopeFilter { sql: "fr.file_id = ?".to_string(), params: vec![Value::Integer(file_id)] };
        let frames = crate::catalog_query::fetch_scoped_frames(&self.db.conn(), &filter).expect("frames");
        crate::catalog_query::file_line(&frames[0])
    }

    fn mark_custom(&self, set_id: i64) {
        self.db.conn().execute("UPDATE frames_set SET is_custom = 1 WHERE id = ?1", [set_id]).expect("custom");
    }
}

#[test]
fn files_by_path_and_handle_select_exactly_those() {
    let c = cat();
    let (f1, _) = c.add(&light("M31/a.fits"));
    let (f2, _) = c.add(&light("M31/b.fits"));
    c.add(&light("M31/c.fits"));
    let by_path = files_scope(&[c.path("M31/a.fits")]);
    assert_eq!(c.scoped_names(&by_path), vec!["a.fits".to_string()]);
    let both = files_scope(&[format!("file:{f1}"), c.path("M31/b.fits")]);
    assert_eq!(c.scoped_names(&both), vec!["a.fits".to_string(), "b.fits".to_string()]);
    assert_eq!(c.resolve(&both).description, "2 named files");
    let one = files_scope(&[format!("file:{f2}")]);
    assert_eq!(c.resolve(&one).description, "1 named file");
    // An empty list is absent.
    assert_eq!(c.scoped_names(&files_scope(&[])).len(), 3);
}

#[test]
fn files_named_as_an_answer_prints_them() {
    let c = cat();
    let (loose, _) = c.add(&light("M31/loose.fits"));
    let (zipped, _) = c.add(&light("M31/zip_member.fits"));
    c.add(&light("M31/other.fits"));
    c.in_zip(zipped);
    let sep = std::path::MAIN_SEPARATOR;
    let zip = c.path("a.zip");
    c.db.conn()
        .execute(
            "UPDATE files SET path = ?2, archive_path_in_zip = 'zip_member.fits' WHERE id = ?1",
            rusqlite::params![zipped, format!("{zip}!{sep}zip_member.fits")],
        )
        .expect("zip path");
    let entries: Vec<String> = [loose, zipped]
        .into_iter()
        .map(|id| {
            let line = c.printed(id);
            format!("{}{}{}", line.folder, sep, line.file_name)
        })
        .collect();
    assert_eq!(c.scoped_names(&files_scope(&entries)), vec!["loose.fits".to_string(), "zip_member.fits".to_string()]);
}

#[test]
fn files_named_by_printed_name_when_it_differs_from_the_path() {
    let c = cat();
    let (id, _) = c.add(&light("M31/stored_as.fits"));
    c.add(&light("M31/other.fits"));
    c.db.conn()
        .execute("UPDATE files SET filename = 'Shown.fits' WHERE id = ?1", [id])
        .expect("rename display name");
    let line = c.printed(id);
    assert_eq!(line.file_name, "Shown.fits");
    let entry = format!("{}{}{}", line.folder, std::path::MAIN_SEPARATOR, line.file_name);
    assert_ne!(entry, c.path("M31/stored_as.fits"), "only the printed-name lookup can find it");
    assert_eq!(c.scoped_names(&files_scope(&[entry])), vec!["Shown.fits".to_string()]);
}

#[test]
fn files_hint_lookups_are_capped() {
    let c = cat();
    c.add(&light("M31/a.fits"));
    let entries: Vec<String> = (0..7).map(|i| c.path(&format!("gone{i}/a.fits"))).collect();
    let r = c.resolve(&files_scope(&entries));
    assert!(r.filter.is_none());
    assert_eq!(r.hints.len(), 7);
    for (i, h) in r.hints.iter().enumerate() {
        assert_eq!(h.condition, "file");
        assert_eq!(h.given, entries[i]);
        if i < 5 {
            assert!(h.looked_up, "hint {i} looks the name up");
            assert_eq!(h.existing, vec![c.path("M31/a.fits")], "hint {i} looks the name up");
        } else {
            assert!(!h.looked_up, "hint {i} is past the lookup cap");
            assert!(h.existing.is_empty(), "hint {i} is past the lookup cap");
        }
    }
    let text = hints_text(&r.hints);
    assert!(text.contains(&format!("no file '{}' (not compared with the catalog: only the first 5 unknown files are)", entries[6])), "{text}");
    assert!(!text.contains(&format!("no file '{}'; nothing like it", entries[6])), "{text}");
}

#[test]
fn files_listed_twice_give_one_hint_and_one_lookup() {
    let c = cat();
    c.add(&light("M31/a.fits"));
    let typo = c.path("gone/a.fits");
    let padded = format!("  {typo} ");
    let r = c.resolve(&files_scope(&[typo.clone(), padded, typo.clone()]));
    assert!(r.filter.is_none());
    assert_eq!(r.hints.len(), 1, "one hint for one typo");
    assert_eq!(r.hints[0].given, typo);
    // Four other unknown entries after the duplicates: the duplicates used one lookup, not three.
    let mut entries = vec![typo.clone(), typo.clone(), typo.clone()];
    entries.extend((0..4).map(|i| c.path(&format!("gone{i}/a.fits"))));
    let r = c.resolve(&files_scope(&entries));
    assert_eq!(r.hints.len(), 5);
    assert!(r.hints.iter().all(|h| h.looked_up), "the duplicates did not eat the lookup budget");
}

#[test]
fn files_narrow_with_other_fields() {
    let c = cat();
    c.add(&light("M31/a.fits"));
    c.add(&Fx { path: "M31/b.fits", imagetyp: Some(ImageType::Dark), ..Default::default() });
    let entries = [c.path("M31/a.fits"), c.path("M31/b.fits")];
    let s = Scope { frame_type: Some("lights".into()), ..files_scope(&entries) };
    assert_eq!(c.scoped_names(&s), vec!["a.fits".to_string()]);
}

#[test]
fn files_one_bad_entry_matches_nothing_and_hints() {
    let c = cat();
    c.add(&light("M31/a.fits"));
    c.add(&light("M31/b.fits"));
    let typo = c.path("M32/a.fits");
    let r = c.resolve(&files_scope(&[c.path("M31/b.fits"), typo.clone()]));
    assert!(r.filter.is_none(), "one bad entry empties the whole scope");
    assert_eq!(r.hints.len(), 1);
    assert_eq!(r.hints[0].condition, "file");
    assert_eq!(r.hints[0].given, typo);
    assert_eq!(r.hints[0].existing, vec![c.path("M31/a.fits")]);
    // A handle no file has.
    let r = c.resolve(&files_scope(&["file:99999".to_string()]));
    assert!(r.filter.is_none());
    assert_eq!(r.hints[0].condition, "file");
}

#[test]
fn files_over_cap_refused() {
    let c = cat();
    let many: Vec<String> = (0..2001).map(|i| format!("file:{i}")).collect();
    let err = resolve_scope(&c.db.conn(), &files_scope(&many)).expect_err("over the cap");
    assert!(err.to_string().contains("at most 2000 files"), "{err}");
    let ok: Vec<String> = (0..2000).map(|i| format!("file:{i}")).collect();
    assert!(resolve_scope(&c.db.conn(), &files_scope(&ok)).is_ok());
}

#[test]
fn frame_set_handle_selects_one_of_two_same_named() {
    let c = cat();
    let (_, a) = c.add(&light("one/a.fits"));
    let (_, b) = c.add(&light("two/b.fits"));
    let s1 = c.set_of(Some("LDN 1272"), false, &[a]);
    let s2 = c.set_of(Some("LDN 1272"), false, &[b]);
    assert_eq!(frame_set_handle(s1), format!("set:{s1}"));
    assert_eq!(parse_frame_set_handle(&frame_set_handle(s2)), Some(s2));
    assert_eq!(parse_frame_set_handle("LDN 1272"), None);
    let by = |id: i64| Scope { frame_set: Some(frame_set_handle(id)), ..Default::default() };
    assert_eq!(c.scoped_names(&by(s1)), vec!["a.fits".to_string()]);
    assert_eq!(c.scoped_names(&by(s2)), vec!["b.fits".to_string()]);
    assert_eq!(c.resolve(&by(s2)).description, format!("frame set set:{s2}"));
    // The name still takes both.
    let named = Scope { frame_set: Some("LDN 1272".into()), ..Default::default() };
    assert_eq!(c.scoped_names(&named), vec!["a.fits".to_string(), "b.fits".to_string()]);
}

#[test]
fn frame_set_handle_unknown_hints() {
    let c = cat();
    let (_, a) = c.add(&light("one/a.fits"));
    c.set_of(Some("LDN 1272"), false, &[a]);
    let r = c.resolve(&Scope { frame_set: Some("set:9999".into()), ..Default::default() });
    assert!(r.filter.is_none());
    assert_eq!(r.hints.len(), 1);
    assert_eq!(r.hints[0].condition, "frame set");
    assert_eq!(r.hints[0].given, "set:9999");
    assert!(r.hints[0].existing.is_empty());
}

#[test]
fn tab_stage_selects_frames_of_non_custom_unarchived_sets() {
    let c = cat();
    let (_, w) = c.add(&light("w/wip.fits"));
    let (_, s) = c.add(&light("s/stage.fits"));
    let (_, a) = c.add(&light("a/archived.fits"));
    let wip = c.set_of(Some("W"), false, &[w]);
    c.mark_custom(wip);
    c.set_of(Some("S"), false, &[s]);
    c.set_of(Some("A"), true, &[a]);
    let tab = |t| Scope { tab: Some(t), ..Default::default() };
    assert_eq!(c.scoped_names(&tab(FrameSetTab::Stage)), vec!["stage.fits".to_string()]);
    assert_eq!(c.scoped_names(&tab(FrameSetTab::Wip)), vec!["wip.fits".to_string()]);
    assert_eq!(c.scoped_names(&tab(FrameSetTab::Archive)), vec!["archived.fits".to_string()]);
    assert_eq!(c.resolve(&tab(FrameSetTab::Stage)).description, "in Stage");
}

#[test]
fn tab_archive_wins_over_custom() {
    let c = cat();
    let (_, a) = c.add(&light("a/archived.fits"));
    let set = c.set_of(Some("A"), true, &[a]);
    c.mark_custom(set);
    let tab = |t| Scope { tab: Some(t), ..Default::default() };
    assert_eq!(c.scoped_names(&tab(FrameSetTab::Archive)), vec!["archived.fits".to_string()]);
    assert_eq!(c.scoped_names(&tab(FrameSetTab::Wip)), Vec::<String>::new());
    assert_eq!(c.scoped_names(&tab(FrameSetTab::Stage)), Vec::<String>::new());
}

#[test]
fn describe_scope_names_files_and_handle() {
    let s = Scope {
        frame_set: Some("set:231".into()),
        tab: Some(FrameSetTab::Wip),
        files: Some(vec!["a".into(), "b".into()]),
        ..Default::default()
    };
    assert_eq!(describe_scope(&s), "frame set set:231, in WIP, 2 named files");
    assert_eq!(describe_scope(&Scope { files: Some(vec!["a".into()]), ..Default::default() }), "1 named file");
}

#[test]
fn describe_scope_counts_a_file_listed_twice_once() {
    let twice = Scope { files: Some(vec!["a".into(), " a ".into(), "a".into()]), ..Default::default() };
    assert_eq!(describe_scope(&twice), "1 named file");
    let two = Scope { files: Some(vec!["a".into(), "b".into(), "a".into()]), ..Default::default() };
    assert_eq!(describe_scope(&two), "2 named files");
}
