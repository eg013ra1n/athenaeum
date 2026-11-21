//! Scoped duplicate and missing-metadata findings against real catalogs.

use super::*;
use crate::api::files::get_duplicates;
use crate::db;
use crate::models::{File, FileFormat, Frame, ImageType, MissingMetadataRow};
use crate::scope::{resolve_scope, FrameState, Scope};
use crate::services::ServiceContext;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::collections::BTreeSet;
use std::path::Path;

// ---- fixtures ----

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn at(date: &str) -> Option<DateTime<Utc>> {
    Some(DateTime::parse_from_rfc3339(date).expect("an RFC 3339 date").with_timezone(&Utc))
}

/// What one catalog row is made of; `put` writes the file, its frame and, when asked, a header and a full-file hash.
#[derive(Clone)]
struct Spec {
    object: Option<&'static str>,
    camera: Option<&'static str>,
    kind: Option<ImageType>,
    is_master: bool,
    date: Option<&'static str>,
    size: i64,
    header: Option<&'static str>,
    strong_hash: Option<&'static str>,
}

fn spec() -> Spec {
    Spec {
        object: Some("M31"),
        camera: Some("CamA"),
        kind: Some(ImageType::Flat),
        is_master: false,
        date: Some("2024-10-05T22:00:00Z"),
        size: 100,
        header: None,
        strong_hash: None,
    }
}

/// Returns (file id, frame id).
fn put(conn: &Connection, dir: &Path, name: &str, s: &Spec) -> (i64, i64) {
    let file_id = db::insert_file(
        conn,
        &File {
            id: None,
            path: text(&dir.join(name)),
            filename: name.to_string(),
            size: s.size,
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
    .expect("file");
    let frame_id = db::insert_frame(
        conn,
        &Frame {
            file_id,
            object: s.object.map(str::to_string),
            instrume: s.camera.map(str::to_string),
            imagetyp: s.kind.clone(),
            is_master: s.is_master,
            date_obs: s.date.and_then(at),
            ..Default::default()
        },
    )
    .expect("frame");
    if let Some(header) = s.header {
        conn.execute(
            "INSERT INTO fits_header (file_id, header, header_fingerprint) VALUES (?1, ?2, ?3)",
            rusqlite::params![file_id, header, crate::fingerprint::compute_header_fingerprint(header)],
        )
        .expect("header");
    }
    if let Some(hash) = s.strong_hash {
        conn.execute("UPDATE files SET strong_hash = ?2 WHERE id = ?1", rusqlite::params![file_id, hash]).expect("hash");
    }
    (file_id, frame_id)
}

fn black_hole(conn: &Connection, file_id: i64) {
    conn.execute(
        "INSERT INTO black_hole (file_id, from_where, moved_at, original_path) VALUES (?1, 'test', 'now', 'x')",
        [file_id],
    )
    .expect("black hole");
}

fn catalog() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    db::schema::init_db(&conn).expect("schema");
    conn
}

// ---- duplicates ----

/// A catalog with two header groups under the `nas` scan root and a master group under the `lib` root.
struct Dups {
    base: std::path::PathBuf,
    // Fields drop in declaration order: the pool holding `catalog.db` open goes before its folder is removed.
    ctx: ServiceContext,
    /// The file id of pair B's copy under `nas/c`.
    c_copy: i64,
    _tmp: tempfile::TempDir,
}

fn dups() -> Dups {
    let (tmp, base) = crate::test_support::canonical_tempdir();
    let ctx = ServiceContext::new_for_tests(base.join("catalog.db"));
    let c_copy;
    {
        let db = ctx.db.get().expect("db");
        let conn = db.conn();
        db::upsert_scan_root(&conn, &text(&base.join("nas")), "normal").expect("nas root");
        db::upsert_scan_root(&conn, &text(&base.join("lib")), "calibration_library").expect("lib root");
        // Pair A: the same flat on two drives.
        let a = Spec { header: Some("HDR-A"), size: 100, ..spec() };
        put(&conn, &base.join("nas").join("a"), "f1.fits", &a);
        put(&conn, &base.join("nas").join("b"), "f1.fits", &Spec { camera: Some("CamOther"), ..a.clone() });
        // Pair B: another frame, another object.
        let b = Spec { header: Some("HDR-B"), object: Some("M42"), size: 200, ..spec() };
        put(&conn, &base.join("nas").join("a"), "g1.fits", &b);
        c_copy = put(&conn, &base.join("nas").join("c"), "g1.fits", &b).0;
        // One file with no twin.
        put(&conn, &base.join("nas").join("a"), "alone.fits", &Spec { header: Some("HDR-C"), ..spec() });
        // Pair C: a built master kept in two folders of the library.
        let m = Spec {
            kind: Some(ImageType::MasterDark),
            is_master: true,
            strong_hash: Some("FULLHASH"),
            size: 400,
            object: None,
            ..spec()
        };
        put(&conn, &base.join("lib").join("one"), "master.xisf", &m);
        put(&conn, &base.join("lib").join("two"), "master.xisf", &m);
    }
    Dups { base, ctx, c_copy, _tmp: tmp }
}

fn resolved(ctx: &ServiceContext, scope: &Scope) -> crate::scope::ResolvedScope {
    let db = ctx.db.get().expect("db");
    let conn = db.conn();
    resolve_scope(&conn, scope).expect("resolve")
}

fn handle_of(g: &crate::models::DuplicateGroup) -> String {
    format!("dup:{}:{}", g.key.expect("a tagged group").hash_type(), g.content_hash)
}

#[test]
fn scoped_duplicates_equal_the_page() {
    let d = dups();
    let page = get_duplicates(&d.ctx).expect("page");
    assert_eq!(page.len(), 3, "two header groups and a master group");

    let (found, total, extra_bytes) = find_duplicates_in_scope(&d.ctx, &resolved(&d.ctx, &Scope::default()), 0).expect("found");
    let want: BTreeSet<String> = page.iter().map(handle_of).collect();
    let got: BTreeSet<String> = found.iter().map(|f| f.handle.clone()).collect();
    assert_eq!(got, want, "an empty scope is the Duplicates page");
    assert_eq!(total, 3);
    assert_eq!(extra_bytes, 100 + 200 + 400, "one extra copy in each group");
    assert!(found.iter().all(|f| f.copies.len() == 2 && f.extra_copies == 1));
    assert_eq!(found.iter().filter(|f| f.key == crate::db::DuplicateKey::Header).count(), 2);
    assert_eq!(found.iter().filter(|f| f.key == crate::db::DuplicateKey::Master).count(), 1);

    // Past the last page: nothing listed, the total still said.
    let (none, total_again, _) = find_duplicates_in_scope(&d.ctx, &resolved(&d.ctx, &Scope::default()), 1).expect("page 2");
    assert!(none.is_empty());
    assert_eq!(total_again, 3);
}

#[test]
fn a_group_is_in_scope_when_one_copy_is_and_every_copy_is_shown() {
    let d = dups();
    // Pair A's two copies are on different cameras (header identity does not look at the frame row).
    let scope = Scope { camera: Some("CamOther".into()), object: Some("M31".into()), ..Default::default() };
    let (found, total, _) = find_duplicates_in_scope(&d.ctx, &resolved(&d.ctx, &scope), 0).expect("found");
    assert_eq!(total, 1);
    assert_eq!(found.len(), 1);
    let cameras: BTreeSet<Option<String>> = found[0].copies.iter().map(|c| c.line.instrume.clone()).collect();
    assert_eq!(cameras, BTreeSet::from([Some("CamA".to_string()), Some("CamOther".to_string())]), "the copy outside the scope is shown too");
    for c in &found[0].copies {
        assert!(c.line.handle.starts_with("file:"));
        assert_eq!(c.line.file_name, "f1.fits");
        assert!(!c.line.folder.is_empty());
        assert_eq!(c.scan_root.as_deref(), Some(text(&d.base.join("nas")).as_str()));
        assert_eq!(c.root_kind.as_deref(), Some("normal"));
    }

    let nothing = Scope { camera: Some("NoSuchCamera".into()), ..Default::default() };
    let (found, total, bytes) = find_duplicates_in_scope(&d.ctx, &resolved(&d.ctx, &nothing), 0).expect("empty");
    assert!(found.is_empty() && total == 0 && bytes == 0);
}

#[test]
fn master_copies_carry_their_role_and_the_folder_inference() {
    let d = dups();
    let (found, _, _) = find_duplicates_in_scope(&d.ctx, &resolved(&d.ctx, &Scope::default()), 0).expect("found");
    let masters = found.iter().find(|f| f.key == crate::db::DuplicateKey::Master).expect("the master group");
    assert_eq!(masters.matched_by, "same full-file hash (masters and processed files)");
    for c in &masters.copies {
        assert_eq!(c.root_kind.as_deref(), Some("calibration_library"));
        let note = c.folder_note.as_deref().expect("a folder of masters only is said so");
        assert!(note.contains("looks like a processing folder"), "{note}");
    }
    let header = found.iter().find(|f| f.key == crate::db::DuplicateKey::Header).expect("a header group");
    assert_eq!(header.matched_by, "same header, size and name (raw sub-frames)");
    assert!(header.copies.iter().all(|c| c.folder_note.is_none()), "raw sub-frames folders are not processing folders");
    assert!(header.handle.starts_with("dup:header:"), "{}", header.handle);
}

#[test]
fn a_black_hole_copy_leaves_its_group() {
    let d = dups();
    {
        let db = d.ctx.db.get().expect("db");
        black_hole(&db.conn(), d.c_copy);
    }
    let (found, total, _) = find_duplicates_in_scope(&d.ctx, &resolved(&d.ctx, &Scope::default()), 0).expect("found");
    // Pair B had two copies; with one rejected it is no duplicate group any more.
    assert_eq!(total, 2);
    assert!(found.iter().all(|f| f.copies.iter().all(|c| c.line.state != FrameState::BlackHole)));
}

// ---- missing metadata ----

/// Eight frames under `m`; the last one is in the Black Hole. Returns the catalog and the Black Hole file id.
fn missing_catalog() -> (Connection, std::path::PathBuf, i64) {
    let conn = catalog();
    let dir = std::path::PathBuf::from(if cfg!(windows) { "C:\\data\\m" } else { "/data/m" });
    let ok = Spec {
        object: Some("M31"),
        camera: Some("CamA"),
        kind: Some(ImageType::Light),
        date: Some("2024-10-05T22:00:00Z"),
        ..spec()
    };
    let set_coords = |conn: &Connection, frame: i64| {
        conn.execute("UPDATE frames SET ra = 10.68, dec = 41.27, objctra = '00 42 44', objctdec = '+41 16 09' WHERE id = ?1", [frame])
            .expect("coords");
    };
    let (_, f1) = put(&conn, &dir, "l_ok.fits", &ok);
    set_coords(&conn, f1);
    // 2 light with no coordinates, 2024
    put(&conn, &dir, "l_nocoord.fits", &Spec { date: Some("2024-10-06T22:00:00Z"), ..ok.clone() });
    // 3 light with no object, 2023, other camera
    let (_, f3) = put(&conn, &dir, "l_noobj.fits", &Spec { object: None, camera: Some("CamB"), date: Some("2023-03-01T22:00:00Z"), ..ok.clone() });
    set_coords(&conn, f3);
    // 4 dark with neither object nor coordinates: not a light, so neither category lists it
    put(&conn, &dir, "d_noobj.fits", &Spec { object: None, kind: Some(ImageType::Dark), ..ok.clone() });
    // 5 flat with no camera
    let (_, f5) = put(&conn, &dir, "f_nocam.fits", &Spec { camera: None, kind: Some(ImageType::Flat), ..ok.clone() });
    set_coords(&conn, f5);
    // 6 bias with no date
    let (_, f6) = put(&conn, &dir, "b_nodate.fits", &Spec { date: None, kind: Some(ImageType::Bias), ..ok.clone() });
    set_coords(&conn, f6);
    // 7 frame of no type
    let (_, f7) = put(&conn, &dir, "x_notype.fits", &Spec { kind: None, ..ok.clone() });
    set_coords(&conn, f7);
    // 8 light with no coordinates, in the Black Hole
    let (bh_file, _) = put(&conn, &dir, "l_bh.fits", &ok);
    black_hole(&conn, bh_file);
    (conn, dir, bh_file)
}

const CATEGORIES: [(MissingCategory, &str); 5] = [
    (MissingCategory::Coordinates, "coordinates"),
    (MissingCategory::Object, "object"),
    (MissingCategory::Date, "datetime"),
    (MissingCategory::Camera, "instrument"),
    (MissingCategory::FrameType, "frametype"),
];

fn ids(rows: &[MissingMetadataRow]) -> BTreeSet<i64> {
    rows.iter().map(|r| r.frame.id.expect("a frame id")).collect()
}

#[test]
fn category_names_are_the_views() {
    for (cat, name) in CATEGORIES {
        assert_eq!(cat.db_name(), name);
    }
}

#[test]
fn missing_in_scope_equals_view_for_every_category() {
    let (conn, _dir, bh_file) = missing_catalog();
    let all_states = resolve_scope(&conn, &Scope { state: Some(FrameState::ALL.to_vec()), ..Default::default() }).expect("all states");
    let default = resolve_scope(&conn, &Scope::default()).expect("default");
    let bh_frame: i64 = conn.query_row("SELECT id FROM frames WHERE file_id = ?1", [bh_file], |r| r.get(0)).expect("bh frame");

    let mut names: Vec<&str> = CATEGORIES.iter().map(|(_, n)| *n).collect();
    names.push("all");
    let mut any_nonempty = 0;
    for name in names {
        let view = ids(&db::get_frames_with_missing_metadata(&conn, name).expect("view"));
        assert!(!view.contains(&bh_frame), "{name}: the view leaves the Black Hole out");
        let everywhere = ids(&db::get_frames_with_missing_metadata_in(&conn, name, all_states.filter.as_ref().unwrap()).expect("all states"));
        let mut everywhere_without_bh = everywhere.clone();
        everywhere_without_bh.remove(&bh_frame);
        assert_eq!(everywhere_without_bh, view, "{name}: every state is the view plus the Black Hole");
        let scoped = ids(&db::get_frames_with_missing_metadata_in(&conn, name, default.filter.as_ref().unwrap()).expect("default"));
        assert_eq!(scoped, view, "{name}: the default scope is the view");
        if !view.is_empty() {
            any_nonempty += 1;
        }
    }
    assert_eq!(any_nonempty, 6, "every category (and `all`) has a row to compare");

    // The finding's counts say the same.
    let found = find_missing_in_scope(&conn, &default, &[], 0).expect("finding");
    for (cat, name) in CATEGORIES {
        let want = db::get_frames_with_missing_metadata(&conn, name).unwrap().len();
        let got = found.counts.iter().find(|(c, _)| *c == cat).map(|(_, n)| *n).expect("a count per category");
        assert_eq!(got, want, "{name}");
    }
}

#[test]
fn missing_for_one_camera_and_year() {
    let (conn, dir, _) = missing_catalog();
    let scope = resolve_scope(&conn, &Scope { camera: Some("CamA".into()), year: Some(2024), ..Default::default() }).expect("scope");
    let found = find_missing_in_scope(&conn, &scope, &[], 0).expect("finding");
    let counts: Vec<(MissingCategory, usize)> = found.counts.clone();
    assert_eq!(
        counts,
        vec![
            (MissingCategory::Coordinates, 1),
            (MissingCategory::Object, 0),
            (MissingCategory::Date, 0),
            (MissingCategory::Camera, 0),
            (MissingCategory::FrameType, 1),
        ]
    );
    assert_eq!(found.by_folder.len(), 1);
    let (folder, n, examples) = &found.by_folder[0];
    assert_eq!(folder, &text(&dir));
    assert_eq!(*n, 2);
    let names: BTreeSet<&str> = examples.iter().map(|l| l.file_name.as_str()).collect();
    assert_eq!(names, BTreeSet::from(["l_nocoord.fits", "x_notype.fits"]));
    assert_eq!(found.page.items.len(), 2);

    // A category asked for alone narrows the list too.
    let only = find_missing_in_scope(&conn, &scope, &[MissingCategory::FrameType], 0).expect("one category");
    assert_eq!(only.counts, vec![(MissingCategory::FrameType, 1)]);
    assert_eq!(only.page.items.len(), 1);
    assert_eq!(only.page.items[0].file_name, "x_notype.fits");

    // A scope that matches nothing finds nothing.
    let none = resolve_scope(&conn, &Scope { camera: Some("NoSuchCamera".into()), ..Default::default() }).expect("scope");
    let found = find_missing_in_scope(&conn, &none, &[], 0).expect("empty");
    assert!(found.counts.iter().all(|(_, n)| *n == 0) && found.by_folder.is_empty() && found.page.items.is_empty());
}

// ---- processing folders ----

#[test]
fn processing_folder_is_said_as_an_inference() {
    let conn = catalog();
    let base = std::path::PathBuf::from(if cfg!(windows) { "C:\\stack" } else { "/stack" });
    let proc = base.join("proc");
    let masters = Spec { kind: Some(ImageType::MasterDark), is_master: true, ..spec() };
    put(&conn, &proc, "m1.xisf", &masters);
    put(&conn, &proc, "m2.xisf", &Spec { kind: Some(ImageType::MasterFlat), ..masters.clone() });
    let note = processing_folder_note(&conn, &text(&proc)).expect("note");
    assert_eq!(
        note.as_deref(),
        Some("this folder holds 2 masters or processed files and no raw lights; it looks like a processing folder")
    );

    // A processed light (flagged as a master) is a processed file, never a raw light (P6). One file reads singular.
    let lone = base.join("lone");
    put(&conn, &lone, "stack.xisf", &Spec { kind: Some(ImageType::Light), is_master: true, ..spec() });
    assert_eq!(
        processing_folder_note(&conn, &text(&lone)).expect("note").as_deref(),
        Some("this folder holds 1 master or processed file and no raw lights; it looks like a processing folder")
    );

    // A raw light next to them: no inference.
    put(&conn, &proc, "l1.fits", &Spec { kind: Some(ImageType::Light), ..spec() });
    assert_eq!(processing_folder_note(&conn, &text(&proc)).expect("note"), None);

    // Only lights, no masters; an unknown folder; a sibling whose name starts the same way.
    let raw = base.join("raw");
    put(&conn, &raw, "l1.fits", &Spec { kind: Some(ImageType::Light), ..spec() });
    assert_eq!(processing_folder_note(&conn, &text(&raw)).expect("note"), None);
    assert_eq!(processing_folder_note(&conn, &text(&base.join("nowhere"))).expect("note"), None);
    let sibling = base.join("lone2");
    put(&conn, &sibling, "l1.fits", &Spec { kind: Some(ImageType::Light), ..spec() });
    assert!(processing_folder_note(&conn, &text(&lone)).expect("note").is_some(), "lone2 is not lone");
}

#[test]
fn processing_folder_note_leaves_the_black_hole_out() {
    let conn = catalog();
    let base = std::path::PathBuf::from(if cfg!(windows) { "C:\\stack" } else { "/stack" });

    // The only raw light is in the Black Hole: the master stands alone, the note is given.
    let gone_light = base.join("gone-light");
    put(&conn, &gone_light, "m1.xisf", &Spec { kind: Some(ImageType::MasterDark), is_master: true, ..spec() });
    let (light_file, _) = put(&conn, &gone_light, "l1.fits", &Spec { kind: Some(ImageType::Light), ..spec() });
    assert_eq!(processing_folder_note(&conn, &text(&gone_light)).expect("note"), None, "the light still counts");
    black_hole(&conn, light_file);
    assert!(processing_folder_note(&conn, &text(&gone_light)).expect("note").is_some());

    // The only master is in the Black Hole and no raw light is left: no note.
    let gone_master = base.join("gone-master");
    let (master_file, _) =
        put(&conn, &gone_master, "m1.xisf", &Spec { kind: Some(ImageType::MasterDark), is_master: true, ..spec() });
    assert!(processing_folder_note(&conn, &text(&gone_master)).expect("note").is_some());
    black_hole(&conn, master_file);
    assert_eq!(processing_folder_note(&conn, &text(&gone_master)).expect("note"), None);
}
