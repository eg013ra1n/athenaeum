//! Project-scoped WBPP export collector (Stage II collaboration "processor
//! payoff", slice 5): merge the received collab contributions with the
//! processor's OWN calibrated-light outputs into one [`ExportData`] per
//! publisher, so the WBPP organizer can lay a per-publisher folder tree under a
//! single project-titled root.
//!
//! Pure catalog/db read — no hub I/O — so it lives in the ungated `export`
//! module and compiles in the headless (`--no-default-features`) build. It
//! reproduces the LIGHT-frames union SQL from the render-gated `api::collab`
//! (rather than importing the private original) for the same reason.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::{anyhow, Result};
use rusqlite::Connection;

use crate::db::collab::{get_project, linked_set_ids};
use crate::db::collab_exchange::contributions_for_project;
use crate::db::light_calibrations::get_light_calibration_for_frame;
use crate::export::models::{
    CalibrationSubgroup, CalibrationSummary, CameraType, ExportData, ExportFrame, ExportGroup,
    MasterCreationPlan,
};

/// Д1/Д2: one [`ExportData`] per publisher — received (non-superseded)
/// contributions grouped by `publisher_display`, plus one dataset for the
/// processor's own calibrated outputs (key = `own_display`). Pure catalog/db
/// read — no hub I/O, ungated.
///
/// BINDING for Tasks 2/4 — do not change the shape without updating the runner
/// and command layers.
#[derive(Debug, Clone)]
pub struct ProjectExportData {
    /// The project title → the export root folder.
    pub title: String,
    /// `(publisher display → folder, dataset)`, own-first then received in
    /// contribution (oldest-first) order.
    pub publishers: Vec<(String, ExportData)>,
}

/// Collect a project's exportable frames: the processor's own calibrated LIGHT
/// outputs (from the project's linked sets) unioned with every received
/// (non-superseded) contribution, partitioned into one dataset per publisher
/// display name.
///
/// `own_display` is resolved by the runner from the cached membership snapshot;
/// it is the folder/dataset key for the processor's own outputs. A received
/// publisher whose display name equals `own_display` merges into the same
/// dataset (dedup rule below still applies per `frame_uuid`).
///
/// Errors: the project row must be in the local cache (`get_project`) — missing
/// ⇒ "project not in the local cache"; a project with no own frames AND no
/// received contributions ⇒ "nothing to export for this project".
pub fn collect_project_export_data(
    conn: &Connection,
    project_id: &str,
    own_display: &str,
) -> Result<ProjectExportData> {
    let project =
        get_project(conn, project_id)?.ok_or_else(|| anyhow!("project not in the local cache"))?;
    let title = project.title;
    let object_name = project.target_name;

    // display name → its frames, insertion-ordered (own first, then received in
    // oldest-first contribution order). A received publisher whose display
    // equals `own_display` merges into the same bucket.
    let mut order: Vec<String> = Vec::new();
    let mut by_display: HashMap<String, Vec<ExportFrame>> = HashMap::new();
    // Every `frame_uuid` already placed — own frames win, so they are added
    // first; a received contribution repeating one is dropped (rule 4).
    let mut seen_uuids: HashSet<String> = HashSet::new();

    // ── Own side first (rule 3/4): the calibrated LIGHT outputs of the linked
    // sets, each with a fresh output file on disk. ──────────────────────────
    let set_ids = linked_set_ids(conn, project_id)?;
    for (frame_id, _filename) in union_light_frames(conn, &set_ids)? {
        let Some(cal) = get_light_calibration_for_frame(conn, frame_id)? else {
            tracing::warn!(
                frame_id,
                "project export: linked light has no calibrated output — skipping"
            );
            continue;
        };
        if !Path::new(&cal.output_path).exists() {
            tracing::warn!(frame_id, path = %cal.output_path, "project export: calibrated output missing on disk — skipping");
            continue;
        }
        let (ef, uuid) = own_export_frame(conn, frame_id, &cal.output_path)?;
        if let Some(u) = uuid.filter(|s| !s.is_empty()) {
            seen_uuids.insert(u);
        }
        add_frame(&mut order, &mut by_display, own_display, ef);
    }

    // ── Received side (rule 2): non-superseded contributions, partitioned by
    // publisher display, deduped against own frames by frame_uuid. ──────────
    for c in contributions_for_project(conn, project_id)? {
        if c.superseded {
            continue;
        }
        if !c.frame_uuid.is_empty() && seen_uuids.contains(&c.frame_uuid) {
            tracing::debug!(
                frame_uuid = %c.frame_uuid,
                publisher = %c.publisher_display,
                "project export: received frame already covered by an own output — skipping"
            );
            continue;
        }
        let meta: serde_json::Value = match serde_json::from_str(&c.frame_meta) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    frame_uuid = %c.frame_uuid,
                    publisher = %c.publisher_display,
                    error = %e,
                    "project export: contribution frame_meta is not valid json — skipping row"
                );
                continue;
            }
        };
        let ef = ExportFrame {
            frame_id: -1,
            file_id: -1,
            file_path: c.landed_path.clone(),
            filename: basename(&c.landed_path),
            exptime: meta_f64(&meta, "exptime"),
            filter: meta_str(&meta, "filter"),
            ccd_temp: meta_f64(&meta, "ccd_temp"),
            gain: meta_f64(&meta, "gain"),
            offset: meta_f64(&meta, "offset"),
            binning: meta_str(&meta, "binning"),
            date_obs: meta_str(&meta, "date_obs"),
            focallen: meta_f64(&meta, "focallen"),
            xpixsz: meta_f64(&meta, "xpixsz"),
            bayerpat: meta_str(&meta, "bayerpat"),
            instrume: meta_str(&meta, "instrume"),
        };
        if !c.frame_uuid.is_empty() {
            seen_uuids.insert(c.frame_uuid.clone());
        }
        add_frame(&mut order, &mut by_display, &c.publisher_display, ef);
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
        "collected project export data"
    );
    Ok(ProjectExportData { title, publishers })
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

/// Build one own-side [`ExportFrame`] from the `frames` row, with the calibrated
/// output as its file path/name. Returns `(frame, frame_uuid)` — the uuid feeds
/// the own-wins dedup.
fn own_export_frame(
    conn: &Connection,
    frame_id: i64,
    output_path: &str,
) -> Result<(ExportFrame, Option<String>)> {
    let filename = basename(output_path);
    let row = conn.query_row(
        "SELECT f.file_id, f.exptime, f.filter, f.ccd_temp, f.gain, f.offset, \
                f.binning, f.date_obs, f.focallen, f.xpixsz, f.bayerpat, f.instrume, f.uuid \
         FROM frames f WHERE f.id = ?1",
        [frame_id],
        |row| {
            let uuid: Option<String> = row.get(12)?;
            Ok((
                ExportFrame {
                    frame_id,
                    file_id: row.get(0)?,
                    file_path: output_path.to_string(),
                    filename: filename.clone(),
                    exptime: row.get(1)?,
                    filter: row.get(2)?,
                    ccd_temp: row.get(3)?,
                    gain: row.get(4)?,
                    offset: row.get(5)?,
                    binning: row.get(6)?,
                    date_obs: row.get(7)?,
                    focallen: row.get(8)?,
                    xpixsz: row.get(9)?,
                    bayerpat: row.get(10)?,
                    instrume: row.get(11)?,
                },
                uuid,
            ))
        },
    )?;
    Ok(row)
}

/// The union of LIGHT `(frame_id, filename)` across many frame sets, deduped by
/// frame id. Reproduced from the render-gated `api::collab::union_light_frames`
/// (this module is ungated and must not import it).
fn union_light_frames(conn: &Connection, set_ids: &[i64]) -> Result<Vec<(i64, String)>> {
    if set_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; set_ids.len()].join(",");
    let sql = format!(
        "SELECT DISTINCT sm.frame_id, fi.filename \
         FROM session_members sm \
         JOIN sessions s ON s.id = sm.session_id \
         JOIN imaging_nights ino ON ino.id = s.imaging_night_id \
         JOIN frames f ON f.id = sm.frame_id \
         JOIN files fi ON fi.id = f.file_id \
         WHERE ino.frames_set_id IN ({placeholders}) AND f.imagetyp = 'Light' \
         ORDER BY sm.frame_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(set_ids.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
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
    use crate::db::collab::{link_set, upsert_project, CollabProjectRow};
    use crate::db::collab_exchange::{
        insert_contribution, upsert_package, ContributionRow, PackageRow,
    };
    use crate::db::light_calibrations::{
        upsert_light_calibration, FlatNormMode, LightCalRow, LIGHT_CAL_ENGINE_VERSION,
    };
    use rusqlite::params;

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
                pending_announcements: 0,
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
                fetched_at: String::new(),
            },
        )
        .unwrap();
    }

    /// Upsert the parent package row a contribution's FK requires.
    fn mk_pkg(conn: &Connection, project_id: &str, package_id: &str, publisher: &str) {
        upsert_package(
            conn,
            &PackageRow {
                package_id: package_id.into(),
                project_id: project_id.into(),
                announcement_id: format!("ann-{package_id}"),
                publisher_display: publisher.into(),
                own: false,
                root_hash: "rh".into(),
                byte_size: 0,
                frame_count: 1,
                manifest_xxh3: None,
                aggregate_stats: "{}".into(),
                supersedes: "[]".into(),
                state: "published".into(),
                reject_reason: None,
                superseded: false,
                origin: "remote".into(),
                local_dir: None,
                manifest_ndjson: None,
                local_status: "none".into(),
                holder_count: 0,
                online_count: 0,
                created_at: "2026-07-13 00:00:00".into(),
                decided_at: None,
                fetched_at: String::new(),
            },
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn mk_contrib(
        conn: &Connection,
        project_id: &str,
        package_id: &str,
        publisher: &str,
        uuid: &str,
        landed_path: &str,
        frame_meta: &str,
        superseded: bool,
    ) {
        insert_contribution(
            conn,
            &ContributionRow {
                id: 0,
                project_id: project_id.into(),
                package_id: package_id.into(),
                frame_uuid: uuid.into(),
                publisher_display: publisher.into(),
                rel_path: format!("{publisher}/{uuid}.fits"),
                landed_path: landed_path.into(),
                byte_size: 1,
                xxh3: format!("h-{uuid}"),
                frame_meta: frame_meta.into(),
                analysis: None,
                superseded,
                created_at: String::new(),
            },
        )
        .unwrap();
    }

    /// A single linked own set with one imaging night + session, returns the
    /// (set_id, session_id) so the caller can add frames.
    fn seed_own_set(conn: &Connection) -> (i64, i64) {
        conn.execute(
            "INSERT INTO frames_set (name, objctra, objctdec) VALUES ('Own Set', '05:35:00', '-05:23:00')",
            [],
        )
        .unwrap();
        let set_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time) \
             VALUES (?1, '2026-07-01T20:00:00Z', '2026-07-02T03:00:00Z')",
            [set_id],
        )
        .unwrap();
        let night = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'CamA')",
            [night],
        )
        .unwrap();
        let session = conn.last_insert_rowid();
        (set_id, session)
    }

    /// Add a LIGHT frame to a session; returns its frame id.
    fn add_own_frame(
        conn: &Connection,
        session: i64,
        uuid: &str,
        instrume: &str,
        filter: &str,
    ) -> i64 {
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format) \
             VALUES (?1, ?2, 1000, '2026-07-01T21:00:00Z', 'FITS')",
            params![format!("/raw/{uuid}.fits"), format!("{uuid}.fits")],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp, object, instrume, exptime, filter, uuid) \
             VALUES (?1, 'Light', 'M42', ?2, 300.0, ?3, ?4)",
            params![file_id, instrume, filter, uuid],
        )
        .unwrap();
        let frame_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            params![session, frame_id],
        )
        .unwrap();
        frame_id
    }

    fn add_cal_row(conn: &Connection, frame_id: i64, output_path: &str) {
        upsert_light_calibration(
            conn,
            &LightCalRow {
                id: 0,
                frame_id: Some(frame_id),
                source_uuid: None,
                source_filename: None,
                output_path: output_path.into(),
                dark_set_id: None,
                flat_set_id: None,
                bias_set_id: None,
                calstat: "B".into(),
                flat_norm_applied: false,
                flat_norm_mode: FlatNormMode::CentralThird.as_wire_str().into(),
                output_hash: "h".into(),
                engine_version: LIGHT_CAL_ENGINE_VERSION,
                created_at: "2026-07-10T00:00:00Z".into(),
                cal_params: "{}".into(),
            },
        )
        .unwrap();
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

    // (a) two publishers, same basename → two datasets, filenames unprefixed.
    #[test]
    fn two_publishers_same_basename_land_in_two_datasets() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        seed_project(&conn, "p-1");
        mk_pkg(&conn, "p-1", "pkg-a", "Alice");
        mk_pkg(&conn, "p-1", "pkg-b", "Bob");
        let a_landed = tmp.path().join("Alice").join("L_0001.fits");
        let b_landed = tmp.path().join("Bob").join("L_0001.fits");
        mk_contrib(
            &conn,
            "p-1",
            "pkg-a",
            "Alice",
            "u-a",
            &a_landed.to_string_lossy(),
            r#"{"instrume":"CamA","filter":"L","exptime":300.0}"#,
            false,
        );
        mk_contrib(
            &conn,
            "p-1",
            "pkg-b",
            "Bob",
            "u-b",
            &b_landed.to_string_lossy(),
            r#"{"instrume":"CamB","filter":"L","exptime":300.0}"#,
            false,
        );

        let data = collect_project_export_data(&conn, "p-1", "Me").unwrap();
        assert_eq!(data.title, "Proj Title");
        assert_eq!(data.publishers.len(), 2, "one dataset per publisher");

        let alice = find(&data, "Alice");
        assert_eq!(
            alice.frame_set_name, "Alice",
            "dataset folder = publisher display"
        );
        let af = all_frames(alice);
        assert_eq!(af.len(), 1);
        assert_eq!(
            af[0].filename, "L_0001.fits",
            "publisher basename, unprefixed"
        );
        assert_eq!(af[0].file_path, a_landed.to_string_lossy());
        assert_eq!(af[0].frame_id, -1, "received frames use sentinel ids");

        let bob = find(&data, "Bob");
        assert_eq!(all_frames(bob)[0].filename, "L_0001.fits");
        assert_eq!(all_frames(bob)[0].file_path, b_landed.to_string_lossy());
    }

    // (a2) one publisher, two filters → two (filter, camera) groups.
    #[test]
    fn one_publisher_two_filters_two_groups() {
        let conn = test_conn();
        seed_project(&conn, "p-1");
        mk_pkg(&conn, "p-1", "pkg-a", "Alice");
        mk_contrib(
            &conn,
            "p-1",
            "pkg-a",
            "Alice",
            "u-1",
            "/land/Alice/l.fits",
            r#"{"instrume":"CamA","filter":"L"}"#,
            false,
        );
        mk_contrib(
            &conn,
            "p-1",
            "pkg-a",
            "Alice",
            "u-2",
            "/land/Alice/r.fits",
            r#"{"instrume":"CamA","filter":"R"}"#,
            false,
        );

        let data = collect_project_export_data(&conn, "p-1", "Me").unwrap();
        assert_eq!(data.publishers.len(), 1);
        let alice = find(&data, "Alice");
        assert_eq!(alice.groups.len(), 2, "two filters → two groups");
        let mut keys: Vec<_> = alice.groups.iter().map(|g| g.group_key.clone()).collect();
        keys.sort();
        assert_eq!(keys, vec!["L_Mono".to_string(), "R_Mono".to_string()]);
    }

    // (b) superseded contribution excluded.
    #[test]
    fn superseded_contribution_excluded() {
        let conn = test_conn();
        seed_project(&conn, "p-1");
        mk_pkg(&conn, "p-1", "pkg-a", "Alice");
        mk_contrib(
            &conn,
            "p-1",
            "pkg-a",
            "Alice",
            "u-live",
            "/land/Alice/live.fits",
            r#"{"instrume":"CamA","filter":"L"}"#,
            false,
        );
        mk_contrib(
            &conn,
            "p-1",
            "pkg-a",
            "Alice",
            "u-old",
            "/land/Alice/old.fits",
            r#"{"instrume":"CamA","filter":"L"}"#,
            true,
        );

        let data = collect_project_export_data(&conn, "p-1", "Me").unwrap();
        let alice = find(&data, "Alice");
        let frames = all_frames(alice);
        assert_eq!(frames.len(), 1, "the superseded row is excluded");
        assert_eq!(frames[0].filename, "live.fits");
    }

    // (c) own calibrated frame lands in own_display dataset; a linked frame with
    // no cal row or a missing output file is skipped.
    #[test]
    fn own_calibrated_frame_lands_missing_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        seed_project(&conn, "p-1");
        let (set_id, session) = seed_own_set(&conn);

        // A: cal row + real file on disk → included.
        let fa = add_own_frame(&conn, session, "own-a", "CamA", "L");
        let a_out = tmp.path().join("c_own-a.fits");
        std::fs::write(&a_out, b"calibrated-a").unwrap();
        add_cal_row(&conn, fa, &a_out.to_string_lossy());

        // B: no cal row → skipped.
        let _fb = add_own_frame(&conn, session, "own-b", "CamA", "L");

        // C: cal row whose output_path does not exist → skipped.
        let fc = add_own_frame(&conn, session, "own-c", "CamA", "L");
        let c_out = tmp.path().join("does-not-exist").join("c_own-c.fits");
        add_cal_row(&conn, fc, &c_out.to_string_lossy());

        link_set(&conn, "p-1", set_id).unwrap();

        let data = collect_project_export_data(&conn, "p-1", "Me").unwrap();
        let me = find(&data, "Me");
        let frames = all_frames(me);
        assert_eq!(
            frames.len(),
            1,
            "only the frame with a fresh output on disk survives"
        );
        assert_eq!(
            frames[0].filename, "c_own-a.fits",
            "plain basename of the calibrated output"
        );
        assert_eq!(frames[0].file_path, a_out.to_string_lossy());
        assert_eq!(
            frames[0].frame_id, fa,
            "own frames carry their real frame id"
        );
        assert_eq!(me.total_light_frames, 1);
    }

    // (d) malformed frame_meta skips only that row.
    #[test]
    fn malformed_frame_meta_skips_only_that_row() {
        let conn = test_conn();
        seed_project(&conn, "p-1");
        mk_pkg(&conn, "p-1", "pkg-a", "Alice");
        mk_contrib(
            &conn,
            "p-1",
            "pkg-a",
            "Alice",
            "u-bad",
            "/land/Alice/bad.fits",
            "{ this is not valid json",
            false,
        );
        mk_contrib(
            &conn,
            "p-1",
            "pkg-a",
            "Alice",
            "u-ok",
            "/land/Alice/ok.fits",
            r#"{"instrume":"CamA","filter":"L"}"#,
            false,
        );

        let data = collect_project_export_data(&conn, "p-1", "Me").unwrap();
        let alice = find(&data, "Alice");
        let frames = all_frames(alice);
        assert_eq!(
            frames.len(),
            1,
            "the malformed row is skipped, the valid one survives"
        );
        assert_eq!(frames[0].filename, "ok.fits");
    }

    // (e) empty project → error.
    #[test]
    fn empty_project_errors() {
        let conn = test_conn();
        seed_project(&conn, "p-1");
        let err = collect_project_export_data(&conn, "p-1", "Me").unwrap_err();
        assert!(err.to_string().contains("nothing to export"), "got {err}");
    }

    // Rule 1: a project not in the local cache errors distinctly.
    #[test]
    fn missing_project_errors() {
        let conn = test_conn();
        let err = collect_project_export_data(&conn, "no-such", "Me").unwrap_err();
        assert!(
            err.to_string().contains("not in the local cache"),
            "got {err}"
        );
    }

    // Own + received sharing a frame_uuid: own wins, received duplicate dropped.
    #[test]
    fn own_wins_over_received_duplicate_uuid() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        seed_project(&conn, "p-1");
        let (set_id, session) = seed_own_set(&conn);
        let fa = add_own_frame(&conn, session, "shared-uuid", "CamA", "L");
        let a_out = tmp.path().join("c_shared.fits");
        std::fs::write(&a_out, b"own-shared").unwrap();
        add_cal_row(&conn, fa, &a_out.to_string_lossy());
        link_set(&conn, "p-1", set_id).unwrap();

        // A received contribution for the SAME uuid, under the same display "Me".
        mk_pkg(&conn, "p-1", "pkg-a", "Me");
        mk_contrib(
            &conn,
            "p-1",
            "pkg-a",
            "Me",
            "shared-uuid",
            "/land/Me/dup.fits",
            r#"{"instrume":"CamA","filter":"L"}"#,
            false,
        );

        let data = collect_project_export_data(&conn, "p-1", "Me").unwrap();
        let me = find(&data, "Me");
        let frames = all_frames(me);
        assert_eq!(
            frames.len(),
            1,
            "the received duplicate uuid is dropped; own wins"
        );
        assert_eq!(frames[0].filename, "c_shared.fits");
    }
}
