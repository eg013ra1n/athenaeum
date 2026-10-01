//! Project-scoped WBPP export collector (collab v3 wave 2, plan P26 / spec
//! amendment A1): every project frame this device holds — the frames I
//! published (`own`) and the replicas I pulled (`replica`) — read from
//! `project_frames_local`, one [`ExportData`] per publisher, so the WBPP
//! organizer can lay a per-publisher folder tree under a single
//! project-titled root. The WBPP hierarchy itself is unchanged in this wave.
//!
//! The path of a project frame comes ONLY from its row's `landed_path` (an
//! own frame may live outside the Collaboration root), and its metadata only
//! from the row's manifest JSON — never a header card, never the folder
//! layout. Pure catalog/db read — no hub I/O — so it lives in the ungated
//! `export` module and compiles in the headless (`--no-default-features`)
//! build.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{anyhow, Result};
use rusqlite::Connection;

use crate::db::collab::get_project;
use crate::db::collab_frames::{list_for_project, FrameOrigin, LocalFrameRow};
use crate::export::models::{
    CalibrationSubgroup, CalibrationSummary, CameraType, ExportData, ExportFrame, ExportGroup,
    MasterCreationPlan,
};

/// One [`ExportData`] per publisher: my own frames under `own_display`, then
/// each replica publisher under its display name.
///
/// BINDING for the runner and command layers — do not change the shape
/// without updating them.
#[derive(Debug, Clone)]
pub struct ProjectExportData {
    /// The project title → the export root folder.
    pub title: String,
    /// `(publisher display → folder, dataset)`, own first, then replica
    /// publishers in first-seen order.
    pub publishers: Vec<(String, ExportData)>,
    /// Human-readable, one-per-skip notes for frames the collector dropped (a
    /// row whose manifest JSON is unreadable). The runner prepends these to
    /// the organizer warnings so `ExportResult.warnings` — and thus the export
    /// dialog — names the omitted frames instead of silently reporting a
    /// smaller file count.
    pub warnings: Vec<String>,
}

/// Collect a project's exportable frames: every `project_frames_local` row
/// that is published, accepted, on disk and not awaiting GC (R31), own and
/// replica alike, at its `landed_path` (P26), partitioned into one dataset
/// per publisher display.
///
/// `own_display` is resolved by the runner from the cached membership
/// snapshot; it is the folder/dataset key for my own frames. A replica
/// publisher whose display equals it merges into the same dataset.
///
/// Errors: the project row must be in the local cache (`get_project`) —
/// missing ⇒ "project not in the local cache"; no exportable frame ⇒
/// "nothing to export for this project".
pub fn collect_project_export_data(
    conn: &Connection,
    project_id: &str,
    own_display: &str,
) -> Result<ProjectExportData> {
    let project =
        get_project(conn, project_id)?.ok_or_else(|| anyhow!("project not in the local cache"))?;
    let title = project.title;
    let object_name = project.target_name;

    let mut rows: Vec<LocalFrameRow> = list_for_project(conn, project_id)?
        .into_iter()
        .filter(exportable)
        .collect();
    // Own first (stable: the table's order within each origin is kept).
    rows.sort_by_key(|r| r.origin != FrameOrigin::Own);

    // display name → its frames, insertion-ordered.
    let mut order: Vec<String> = Vec::new();
    let mut by_display: HashMap<String, Vec<ExportFrame>> = HashMap::new();
    let mut warnings: Vec<String> = Vec::new();

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
        add_frame(
            &mut order,
            &mut by_display,
            &display,
            export_frame(&landed, &manifest),
        );
    }

    if order.is_empty() {
        return Err(anyhow!("nothing to export for this project"));
    }

    let mut publishers = Vec::with_capacity(order.len());
    for display in order {
        let frames = by_display.remove(&display).unwrap_or_default();
        let data = build_dataset(&display, &object_name, frames);
        publishers.push((display, data));
    }
    tracing::info!(
        project_id,
        publishers = publishers.len(),
        skipped = warnings.len(),
        "collected project export data"
    );
    Ok(ProjectExportData {
        title,
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

/// One [`ExportFrame`] for a project frame: the file at `landed`, its
/// metadata from the manifest row (top-level `filterRaw`/`exptimeSec`/
/// `dateObs`, the rest from `meta`). Project frames carry no catalog ids.
fn export_frame(landed: &str, manifest: &serde_json::Value) -> ExportFrame {
    let meta = manifest
        .get("meta")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let filter = meta_str(manifest, "filterRaw")
        .filter(|f| !f.is_empty())
        .or_else(|| meta_str(manifest, "filterCanonical").filter(|f| !f.is_empty()));
    ExportFrame {
        frame_id: -1,
        file_id: -1,
        file_path: landed.to_string(),
        filename: basename(landed),
        exptime: meta_f64(manifest, "exptimeSec"),
        filter,
        ccd_temp: None,
        gain: None,
        offset: None,
        binning: meta
            .get("xbinning")
            .and_then(|b| b.as_i64())
            .map(|b| format!("{b}x{b}")),
        date_obs: meta_str(manifest, "dateObs"),
        focallen: meta_f64(&meta, "focalLen"),
        xpixsz: None,
        bayerpat: meta_str(&meta, "bayerpat"),
        instrume: meta_str(&meta, "instrume"),
        debayer_calibrated: None,
    }
}

/// Append `ef` to the bucket for `display`, registering the display's insertion
/// order on first use.
fn add_frame(
    order: &mut Vec<String>,
    by_display: &mut HashMap<String, Vec<ExportFrame>>,
    display: &str,
    ef: ExportFrame,
) {
    if !by_display.contains_key(display) {
        order.push(display.to_string());
        by_display.insert(display.to_string(), Vec::new());
    }
    by_display
        .get_mut(display)
        .expect("bucket just inserted")
        .push(ef);
}

/// Build one publisher dataset: group by `(filter, camera_type)`, one subgroup
/// per `instrume` within each group (so the organizer lays a `camera_<instrume>`
/// folder per camera). Subgroups carry no calibration nodes.
fn build_dataset(display: &str, object_name: &str, frames: Vec<ExportFrame>) -> ExportData {
    // group_key → (filter, camera_type, frames). BTreeMap for deterministic order.
    let mut groups_map: BTreeMap<String, (Option<String>, CameraType, Vec<ExportFrame>)> =
        BTreeMap::new();
    for f in frames {
        let camera = CameraType::from_bayerpat(f.bayerpat.as_deref());
        let key = ExportGroup::make_group_key(f.filter.as_deref(), &camera);
        groups_map
            .entry(key)
            .or_insert_with(|| (f.filter.clone(), camera.clone(), Vec::new()))
            .2
            .push(f);
    }

    let mut groups = Vec::with_capacity(groups_map.len());
    let mut total_light_frames = 0i32;
    let mut total_exposure_seconds = 0f64;

    for (group_key, (filter, camera, gframes)) in groups_map {
        // One subgroup per instrume (deterministic order via BTreeMap; "" key =
        // no instrume → "unknown" camera folder derived by the organizer).
        let mut sub_map: BTreeMap<String, Vec<ExportFrame>> = BTreeMap::new();
        for f in gframes {
            let inst = f.instrume.clone().unwrap_or_default();
            sub_map.entry(inst).or_default().push(f);
        }

        let display_name = ExportGroup::make_display_name(filter.as_deref(), &camera);
        let mut subgroups = Vec::with_capacity(sub_map.len());
        let mut group_frames = 0i32;
        let mut group_exposure = 0f64;

        for (inst, sframes) in sub_map {
            group_frames += sframes.len() as i32;
            group_exposure += sframes.iter().filter_map(|f| f.exptime).sum::<f64>();
            subgroups.push(CalibrationSubgroup {
                subgroup_key: format!("{group_key}__{inst}"),
                display_name: if inst.is_empty() {
                    "Default".to_string()
                } else {
                    inst
                },
                frames: sframes,
                flat: None,
                dark: None,
                bias: None,
                warnings: Vec::new(),
            });
        }

        total_light_frames += group_frames;
        total_exposure_seconds += group_exposure;
        groups.push(ExportGroup {
            group_key,
            filter,
            camera_type: camera,
            display_name,
            subgroups,
            total_frames: group_frames,
            total_exposure: group_exposure,
            warnings: Vec::new(),
        });
    }

    ExportData {
        frame_set_id: -1,
        frame_set_name: display.to_string(),
        object_name: Some(object_name.to_string()),
        groups,
        master_plan: MasterCreationPlan::default(),
        filters: Vec::new(),
        calibration_summary: CalibrationSummary {
            flat_count: 0,
            dark_count: 0,
            bias_count: 0,
            dark_flat_count: 0,
            flats_complete: false,
            darks_complete: false,
            bias_complete: false,
            warnings: Vec::new(),
        },
        total_light_frames,
        total_exposure_seconds,
    }
}

/// The trailing path component of `path`, or the whole string when it has none.
fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Read a JSON number field as `f64` (absent / non-numeric ⇒ `None`).
fn meta_f64(v: &serde_json::Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| x.as_f64())
}

/// Read a JSON string field (absent / non-string ⇒ `None`).
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
    ) -> LocalFrameRow {
        let row = LocalFrameRow {
            project_id: project_id.into(),
            frame_uuid: uuid.into(),
            content_version: 1,
            origin,
            publisher_account_id: format!("acc-{publisher}"),
            publisher_display: publisher.into(),
            file_name: format!("{uuid}.fits"),
            filter_canonical: "L".into(),
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

    fn find<'a>(data: &'a ProjectExportData, display: &str) -> &'a ExportData {
        &data
            .publishers
            .iter()
            .find(|(d, _)| d == display)
            .unwrap_or_else(|| {
                panic!(
                    "no dataset for {display}; have {:?}",
                    data.publishers.iter().map(|(d, _)| d).collect::<Vec<_>>()
                )
            })
            .1
    }

    /// The flat list of every ExportFrame in a dataset (across groups/subgroups).
    fn all_frames(d: &ExportData) -> Vec<&ExportFrame> {
        d.groups
            .iter()
            .flat_map(|g| g.subgroups.iter().flat_map(|s| s.frames.iter()))
            .collect()
    }

    // Two publishers, same basename → two datasets, filenames unprefixed, the
    // file path taken from the row's `landed_path` (P26).
    #[test]
    fn two_publishers_same_basename_land_in_two_datasets() {
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
        );
        mk_frame(
            &conn,
            "p-1",
            "u-b",
            FrameOrigin::Replica,
            "Bob",
            Some(&b),
            m,
        );

        let data = collect_project_export_data(&conn, "p-1", "Me").unwrap();
        assert_eq!(data.title, "Proj Title");
        assert_eq!(data.publishers.len(), 2, "one dataset per publisher");
        let alice = find(&data, "Alice");
        assert_eq!(alice.frame_set_name, "Alice");
        let af = all_frames(alice);
        assert_eq!(af.len(), 1);
        assert_eq!(af[0].filename, "L_0001.fits");
        assert_eq!(af[0].file_path, a);
        assert_eq!(af[0].frame_id, -1, "project frames use sentinel ids");
        assert_eq!(all_frames(find(&data, "Bob"))[0].file_path, b);
    }

    // Own frames export too — under `own_display`, first, from wherever they
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
        );
        mk_frame(
            &conn,
            "p-1",
            "o-1",
            FrameOrigin::Own,
            "My Name",
            Some("/elsewhere/originals/o1.fits"),
            manifest("L", "CamMe"),
        );

        let data = collect_project_export_data(&conn, "p-1", "Me").unwrap();
        let order: Vec<&str> = data.publishers.iter().map(|(d, _)| d.as_str()).collect();
        assert_eq!(order, vec!["Me", "Alice"], "own first, under own_display");
        let mine = all_frames(find(&data, "Me"));
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].file_path, "/elsewhere/originals/o1.fits");
        assert!(data.warnings.is_empty(), "{:?}", data.warnings);
    }

    // Only published frames on disk AND accepted by the gate export (R31): a
    // row with no landed path, one not on disk, a gate-rejected one, one
    // awaiting GC, and an own pending or moderator-rejected frame are skipped.
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
        );
        mk_frame(
            &conn,
            "p-1",
            "pending",
            FrameOrigin::Replica,
            "Alice",
            None,
            m.clone(),
        );
        mk_frame(
            &conn,
            "p-1",
            "gone",
            FrameOrigin::Replica,
            "Alice",
            Some("/c/gone.fits"),
            m.clone(),
        );
        mk_frame(
            &conn,
            "p-1",
            "rejected",
            FrameOrigin::Replica,
            "Alice",
            Some("/c/rej.fits"),
            m,
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
            "no own dataset: nothing own is published"
        );
        let frames = all_frames(find(&data, "Alice"));
        let names: Vec<&str> = frames.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(names, vec!["live.fits"]);
    }

    // A row whose manifest JSON does not parse is skipped — only that row —
    // with one warning naming it, so the export dialog reports the omission.
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
        );
        mk_frame(
            &conn,
            "p-1",
            "bad",
            FrameOrigin::Replica,
            "Alice",
            Some("/c/bad.fits"),
            manifest("L", "CamA"),
        );
        conn.execute(
            "UPDATE project_frames_local SET manifest_json = '{not json' WHERE frame_uuid = 'bad'",
            [],
        )
        .unwrap();

        let data = collect_project_export_data(&conn, "p-1", "Me").unwrap();
        let frames = all_frames(find(&data, "Alice"));
        let names: Vec<&str> = frames.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(names, vec!["good.fits"]);
        assert_eq!(
            data.warnings,
            vec!["skipped bad.fits: unreadable frame metadata".to_string()]
        );
    }

    // Frame metadata comes from the manifest row, never a header card: one
    // publisher with two filters lands in two (filter, camera) groups, and the
    // numeric/camera fields are read from `meta`.
    #[test]
    fn metadata_comes_from_the_manifest_row() {
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
        );
        let mut osc = manifest("R", "CamA");
        osc["meta"]["bayerpat"] = serde_json::json!("RGGB");
        mk_frame(
            &conn,
            "p-1",
            "u-r",
            FrameOrigin::Replica,
            "Alice",
            Some("/c/r.fits"),
            osc,
        );

        let data = collect_project_export_data(&conn, "p-1", "Me").unwrap();
        let alice = find(&data, "Alice");
        let mut keys: Vec<_> = alice.groups.iter().map(|g| g.group_key.clone()).collect();
        keys.sort();
        assert_eq!(keys, vec!["L_Mono".to_string(), "R_OSC".to_string()]);
        let l = all_frames(alice)
            .into_iter()
            .find(|f| f.filename == "l.fits")
            .unwrap();
        assert_eq!(l.exptime, Some(300.0));
        assert_eq!(l.filter.as_deref(), Some("L"));
        assert_eq!(l.instrume.as_deref(), Some("CamA"));
        assert_eq!(l.focallen, Some(530.0));
        assert_eq!(l.date_obs.as_deref(), Some("2026-07-01T21:00:00Z"));
        assert_eq!(l.binning.as_deref(), Some("1x1"));
        assert_eq!(alice.total_light_frames, 2);
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
