//! Project-scoped WBPP export collector (collab v3 wave 2, plan P26 / spec
//! amendment A1): every project frame this device holds, the frames I
//! published (`own`) and the replicas I pulled (`replica`), read from
//! `project_frames_local` and partitioned per publisher, so the organizer can
//! lay a per-publisher folder tree under a single project-titled root.
//! Wave 6 (spec section 10.1): the filter is the row's canonical (remaps
//! included), never `filterRaw`.
//!
//! The path of a project frame comes ONLY from its row's `landed_path` (an
//! own frame may live outside the Collaboration root), and its camera only
//! from the row's manifest JSON, never a header card, never the folder
//! layout. Pure catalog/db read, no hub I/O, so it lives in the ungated
//! `export` module and compiles in the headless (`--no-default-features`)
//! build.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Result};
use rusqlite::Connection;

use crate::db::collab::get_project;
use crate::db::collab_frames::{list_for_project, FrameOrigin, LocalFrameRow};

/// One exportable project frame.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectExportFrame {
    pub frame_uuid: String,
    pub landed_path: String,
    /// The landed file's basename (the export keeps it).
    pub file_name: String,
    pub instrume: Option<String>,
    /// The canonical filter (`project_frames_local.filter_canonical`).
    pub filter: String,
    /// The hub's byte size of the current content version.
    pub byte_size: i64,
}

/// A project's exportable frames, per publisher.
///
/// BINDING for the runner and organizer: do not change the shape without
/// updating them.
#[derive(Debug, Clone)]
pub struct ProjectExportData {
    /// The project title, the export root folder.
    pub title: String,
    /// `(publisher display, frames)`, own first, then replica publishers in
    /// first-seen order.
    pub publishers: Vec<(String, Vec<ProjectExportFrame>)>,
    /// One note per frame the collector dropped (a row whose manifest JSON is
    /// unreadable), so the export dialog names the omission.
    pub warnings: Vec<String>,
}

/// Collect a project's exportable frames: every `project_frames_local` row
/// that is published, accepted, on disk and not awaiting GC (R31), own and
/// replica alike, at its `landed_path` (P26), partitioned by publisher display.
///
/// `own_display` is the bucket key for my own frames; a replica publisher
/// whose display equals it merges into the same bucket.
///
/// Errors: the project row must be in the local cache (`get_project`), else
/// "project not in the local cache"; no exportable frame gives "nothing to
/// export for this project".
pub fn collect_project_export_data(
    conn: &Connection,
    project_id: &str,
    own_display: &str,
) -> Result<ProjectExportData> {
    let project =
        get_project(conn, project_id)?.ok_or_else(|| anyhow!("project not in the local cache"))?;
    let mut rows: Vec<LocalFrameRow> = list_for_project(conn, project_id)?
        .into_iter()
        .filter(exportable)
        .collect();
    // Own first (stable: the table's order within each origin is kept).
    rows.sort_by_key(|r| r.origin != FrameOrigin::Own);
    let mut order: Vec<String> = Vec::new();
    let mut by_display: HashMap<String, Vec<ProjectExportFrame>> = HashMap::new();
    let mut warnings = Vec::new();
    for row in rows {
        let display = match row.origin {
            FrameOrigin::Own => own_display.to_string(),
            FrameOrigin::Replica => row.publisher_display.clone(),
        };
        let manifest: serde_json::Value = match serde_json::from_str(&row.manifest_json) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    project_id,
                    frame_uuid = %row.frame_uuid,
                    error = %e,
                    "project export: manifest row is not valid json; frame skipped"
                );
                warnings.push(format!(
                    "skipped {}: unreadable frame metadata",
                    row.file_name
                ));
                continue;
            }
        };
        let landed = row.landed_path.clone().unwrap_or_default();
        let meta = manifest
            .get("meta")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let frame = ProjectExportFrame {
            frame_uuid: row.frame_uuid.clone(),
            file_name: basename(&landed),
            landed_path: landed,
            instrume: meta_str(&meta, "instrume"),
            filter: row.filter_canonical.clone(),
            byte_size: row.byte_size,
        };
        if !by_display.contains_key(&display) {
            order.push(display.clone());
        }
        by_display.entry(display).or_default().push(frame);
    }
    if order.is_empty() {
        return Err(anyhow!("nothing to export for this project"));
    }
    let publishers: Vec<(String, Vec<ProjectExportFrame>)> = order
        .into_iter()
        .map(|d| {
            let frames = by_display.remove(&d).unwrap_or_default();
            (d, frames)
        })
        .collect();
    tracing::info!(
        project_id,
        publishers = publishers.len(),
        skipped = warnings.len(),
        "collected project export data"
    );
    Ok(ProjectExportData {
        title: project.title,
        publishers,
        warnings,
    })
}

/// Is a frame part of the project export (R31)? Published by the project
/// (never an own pending or rejected frame), accepted by the gate, on disk at
/// a recorded path, and not waiting for store GC.
fn exportable(r: &LocalFrameRow) -> bool {
    r.state == "published" && r.accepted && r.on_disk && !r.awaiting_gc && r.landed_path.is_some()
}

/// The trailing path component of `path`, or the whole string when it has none.
fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Read a JSON string field (absent / non-string gives `None`).
fn meta_str(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::collab::{upsert_project, CollabProjectRow};
    use crate::db::collab_frames::{record_own, FrameOrigin, LocalFrameRow, LocalState};

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn
    }

    /// Cache a project row directly (title "Proj Title", target M42).
    fn seed_project(conn: &Connection, project_id: &str) {
        upsert_project(
            conn,
            &CollabProjectRow {
                project_id: project_id.into(),
                slug: "slug".into(),
                title: "Proj Title".into(),
                data_role: "send_receive".into(),
                is_coordinator: true,
                require_approval: false,
                pending_frames: 0,
                project_status: "active".into(),
                target_name: "M42".into(),
                target_ra_deg: 83.8,
                target_dec_deg: -5.4,
                target_radius_deg: 1.0,
                membership_version: 1,
                snapshot_payload_b64: "x".into(),
                snapshot_signature_b64: "x".into(),
                members_json: "[]".into(),
                thresholds_version: None,
                thresholds_rules_json: None,
                gov_caps_json: "[]".into(),
                // all ignored on write (local preference / sync-state / dictionary)
                auto_replicate: true,
                synced_caps_json: "[]".into(),
                hub_version: 0,
                manifest_cursor: 0,
                dictionary_version: None,
                dictionary_json: None,
                policy_json: r#"{"mode":"all"}"#.into(),
                replication_paused: false,
                publish_mode: crate::db::collab::PublishMode::Automatic,
                fetched_at: String::new(),
                feed_epoch: None,
                holder_seq: -1,
            },
        )
        .unwrap();
    }

    /// One `project_frames_local` row: landed at `landed`, `on_disk`,
    /// `accepted`, with `manifest` as its verbatim manifest JSON.
    fn mk_frame(
        conn: &Connection,
        project_id: &str,
        uuid: &str,
        origin: FrameOrigin,
        publisher: &str,
        landed: Option<&str>,
        manifest: serde_json::Value,
        filter_canonical: &str,
    ) -> LocalFrameRow {
        let row = LocalFrameRow {
            project_id: project_id.into(),
            frame_uuid: uuid.into(),
            content_version: 1,
            origin,
            publisher_account_id: format!("acc-{publisher}"),
            publisher_display: publisher.into(),
            file_name: format!("{uuid}.fits"),
            filter_canonical: filter_canonical.into(),
            state: "published".into(),
            accepted: true,
            byte_size: 1,
            xxh3: format!("h-{uuid}"),
            blake3: "b".repeat(64),
            manifest_version: 1,
            manifest_json: manifest.to_string(),
            landed_path: landed.map(str::to_string),
            size_mtime_seen: None,
            on_disk: landed.is_some(),
            awaiting_gc: false,
            source_frame_id: None,
            recipe_hash: None,
            last_error: None,
            updated_at: String::new(),
            local_state: match (origin, landed.is_some()) {
                (FrameOrigin::Own, true) => LocalState::OwnHeld,
                (FrameOrigin::Own, false) => LocalState::OwnMissing,
                (FrameOrigin::Replica, true) => LocalState::Held,
                (FrameOrigin::Replica, false) => LocalState::Wanted,
            },
            frame_seq: None,
        };
        record_own(conn, &row).unwrap();
        row
    }

    fn manifest(filter: &str, instrume: &str) -> serde_json::Value {
        serde_json::json!({
            "filterRaw": filter,
            "exptimeSec": 300.0,
            "dateObs": "2026-07-01T21:00:00Z",
            "meta": {"instrume": instrume, "xbinning": 1, "focalLen": 530.0}
        })
    }

    fn find<'a>(d: &'a ProjectExportData, display: &str) -> &'a [ProjectExportFrame] {
        &d.publishers
            .iter()
            .find(|(p, _)| p == display)
            .expect("publisher present")
            .1
    }

    // Two publishers, same basename → two publisher buckets, file name
    // unprefixed, the path taken from the row's `landed_path` (P26).
    #[test]
    fn two_publishers_same_basename_land_in_two_buckets() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        seed_project(&conn, "p-1");
        let a = tmp.path().join("Alice").join("L_0001.fits");
        let b = tmp.path().join("Bob").join("L_0001.fits");
        let (a, b) = (a.to_string_lossy(), b.to_string_lossy());
        let m = manifest("L", "CamA");
        mk_frame(
            &conn,
            "p-1",
            "u-a",
            FrameOrigin::Replica,
            "Alice",
            Some(&a),
            m.clone(),
            "L",
        );
        mk_frame(
            &conn,
            "p-1",
            "u-b",
            FrameOrigin::Replica,
            "Bob",
            Some(&b),
            m,
            "L",
        );

        let data = collect_project_export_data(&conn, "p-1", "Me").unwrap();
        assert_eq!(data.title, "Proj Title");
        assert_eq!(data.publishers.len(), 2, "one bucket per publisher");
        let af = find(&data, "Alice");
        assert_eq!(af.len(), 1);
        assert_eq!(af[0].file_name, "L_0001.fits");
        assert_eq!(af[0].landed_path, a);
        assert_eq!(af[0].frame_uuid, "u-a");
        assert_eq!(find(&data, "Bob")[0].landed_path, b);
    }

    // Own frames export too, under `own_display`, first, from wherever they
    // live (an own frame may sit outside the Collaboration root, A1).
    #[test]
    fn own_and_replica_frames_are_both_exported() {
        let conn = test_conn();
        seed_project(&conn, "p-1");
        mk_frame(
            &conn,
            "p-1",
            "r-1",
            FrameOrigin::Replica,
            "Alice",
            Some("/collab/m42/Alice/r1.fits"),
            manifest("L", "CamA"),
            "L",
        );
        mk_frame(
            &conn,
            "p-1",
            "o-1",
            FrameOrigin::Own,
            "My Name",
            Some("/elsewhere/originals/o1.fits"),
            manifest("L", "CamMe"),
            "L",
        );

        let data = collect_project_export_data(&conn, "p-1", "Me").unwrap();
        let order: Vec<&str> = data.publishers.iter().map(|(d, _)| d.as_str()).collect();
        assert_eq!(order, vec!["Me", "Alice"], "own first, under own_display");
        let mine = find(&data, "Me");
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].landed_path, "/elsewhere/originals/o1.fits");
        assert_eq!(mine[0].instrume.as_deref(), Some("CamMe"));
        assert!(data.warnings.is_empty(), "{:?}", data.warnings);
    }

    // Only published frames on disk AND accepted by the gate export (R31).
    #[test]
    fn only_published_on_disk_accepted_frames_are_exported() {
        let conn = test_conn();
        seed_project(&conn, "p-1");
        let m = manifest("L", "CamA");
        mk_frame(
            &conn,
            "p-1",
            "live",
            FrameOrigin::Replica,
            "Alice",
            Some("/c/live.fits"),
            m.clone(),
            "L",
        );
        mk_frame(
            &conn,
            "p-1",
            "pending",
            FrameOrigin::Replica,
            "Alice",
            None,
            m.clone(),
            "L",
        );
        mk_frame(
            &conn,
            "p-1",
            "gone",
            FrameOrigin::Replica,
            "Alice",
            Some("/c/gone.fits"),
            m.clone(),
            "L",
        );
        mk_frame(
            &conn,
            "p-1",
            "rejected",
            FrameOrigin::Replica,
            "Alice",
            Some("/c/rej.fits"),
            m,
            "L",
        );
        conn.execute(
            "UPDATE project_frames_local SET on_disk = 0 WHERE frame_uuid = 'gone'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE project_frames_local SET accepted = 0 WHERE frame_uuid = 'rejected'",
            [],
        )
        .unwrap();
        for (uuid, path) in [
            ("gc", "/c/gc.fits"),
            ("own-pending", "/o/pending.fits"),
            ("own-rejected", "/o/rejected.fits"),
        ] {
            let origin = if uuid == "gc" {
                FrameOrigin::Replica
            } else {
                FrameOrigin::Own
            };
            mk_frame(
                &conn,
                "p-1",
                uuid,
                origin,
                "Alice",
                Some(path),
                manifest("L", "CamA"),
                "L",
            );
        }
        conn.execute(
            "UPDATE project_frames_local SET awaiting_gc = 1 WHERE frame_uuid = 'gc'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE project_frames_local SET state = 'pending' WHERE frame_uuid = 'own-pending'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE project_frames_local SET state = 'rejected' WHERE frame_uuid = 'own-rejected'",
            [],
        )
        .unwrap();

        let data = collect_project_export_data(&conn, "p-1", "Me").unwrap();
        assert_eq!(
            data.publishers.len(),
            1,
            "no own bucket: nothing own is published"
        );
        let names: Vec<&str> = find(&data, "Alice")
            .iter()
            .map(|f| f.file_name.as_str())
            .collect();
        assert_eq!(names, vec!["live.fits"]);
    }

    // A row whose manifest JSON does not parse is skipped, only that row,
    // with one warning naming it.
    #[test]
    fn malformed_manifest_row_skips_only_that_row() {
        let conn = test_conn();
        seed_project(&conn, "p-1");
        mk_frame(
            &conn,
            "p-1",
            "good",
            FrameOrigin::Replica,
            "Alice",
            Some("/c/good.fits"),
            manifest("L", "CamA"),
            "L",
        );
        mk_frame(
            &conn,
            "p-1",
            "bad",
            FrameOrigin::Replica,
            "Alice",
            Some("/c/bad.fits"),
            manifest("L", "CamA"),
            "L",
        );
        conn.execute(
            "UPDATE project_frames_local SET manifest_json = '{not json' WHERE frame_uuid = 'bad'",
            [],
        )
        .unwrap();

        let data = collect_project_export_data(&conn, "p-1", "Me").unwrap();
        let names: Vec<&str> = find(&data, "Alice")
            .iter()
            .map(|f| f.file_name.as_str())
            .collect();
        assert_eq!(names, vec!["good.fits"]);
        assert_eq!(
            data.warnings,
            vec!["skipped bad.fits: unreadable frame metadata".to_string()]
        );
    }

    // Camera and size come from the row/manifest, never a header card.
    #[test]
    fn metadata_comes_from_the_row_and_manifest() {
        let conn = test_conn();
        seed_project(&conn, "p-1");
        mk_frame(
            &conn,
            "p-1",
            "u-l",
            FrameOrigin::Replica,
            "Alice",
            Some("/c/l.fits"),
            manifest("L", "CamA"),
            "L",
        );
        let mut m = manifest("R", "CamB");
        m["meta"]["bayerpat"] = serde_json::json!("RGGB");
        mk_frame(
            &conn,
            "p-1",
            "u-r",
            FrameOrigin::Replica,
            "Alice",
            Some("/c/r.fits"),
            m,
            "R",
        );
        let mut noinst = manifest("G", "x");
        noinst["meta"] = serde_json::json!({});
        mk_frame(
            &conn,
            "p-1",
            "u-n",
            FrameOrigin::Replica,
            "Alice",
            Some("/c/n.fits"),
            noinst,
            "G",
        );

        let data = collect_project_export_data(&conn, "p-1", "Me").unwrap();
        let frames = find(&data, "Alice");
        assert_eq!(frames.len(), 3);
        let l = frames.iter().find(|f| f.file_name == "l.fits").unwrap();
        assert_eq!(l.instrume.as_deref(), Some("CamA"));
        assert_eq!(l.filter, "L");
        assert_eq!(l.byte_size, 1);
        let r = frames.iter().find(|f| f.file_name == "r.fits").unwrap();
        assert_eq!(r.instrume.as_deref(), Some("CamB"));
        assert_eq!(r.filter, "R");
        let n = frames.iter().find(|f| f.file_name == "n.fits").unwrap();
        assert_eq!(n.instrume, None);
    }

    #[test]
    fn the_filter_is_the_row_canonical_never_filter_raw() {
        let conn = test_conn();
        seed_project(&conn, "p");
        mk_frame(
            &conn,
            "p",
            "u1",
            FrameOrigin::Replica,
            "Alice",
            Some("/c/a.fits"),
            manifest("Red", "CamA"),
            "R",
        );
        let d = collect_project_export_data(&conn, "p", "Me").unwrap();
        let f = &find(&d, "Alice")[0];
        assert_eq!(f.filter, "R");
        assert_eq!(f.instrume.as_deref(), Some("CamA"));
        assert_eq!(f.landed_path, "/c/a.fits");
        assert_eq!(f.file_name, "a.fits");
        assert_eq!(f.frame_uuid, "u1");
    }

    #[test]
    fn empty_project_errors() {
        let conn = test_conn();
        seed_project(&conn, "p-1");
        let err = collect_project_export_data(&conn, "p-1", "Me").unwrap_err();
        assert!(err.to_string().contains("nothing to export"), "got {err}");
    }

    #[test]
    fn missing_project_errors() {
        let conn = test_conn();
        let err = collect_project_export_data(&conn, "nope", "Me").unwrap_err();
        assert!(
            err.to_string().contains("not in the local cache"),
            "got {err}"
        );
    }
}
