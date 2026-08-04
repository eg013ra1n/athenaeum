//! Frame-set–oriented export queries for the export UI.
//!
//! Both host wrappers (`commands/export.rs`, `routes/export.rs`) previously
//! carried byte-identical copies of these two bodies — including two separate
//! definitions of `ExportableFrameSet`. They now live here once and the
//! wrappers delegate.

use anyhow::Result;
use rusqlite::Connection;

use crate::export::data_collector::collect_export_data;
use crate::export::models::{
    CalibrationRoute, CalibrationRouteGroup, CalibrationRouteSummary, CalibrationTreeNode,
};

/// Frame set summary for export selection
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportableFrameSet {
    pub id: i64,
    pub name: Option<String>,
    pub total_exposure_seconds: f64,
    pub frame_count: i32,
    pub object_name: Option<String>,
    pub filters: Vec<String>,
}

/// Get a list of available frame sets for export
pub fn get_exportable_frame_sets(conn: &Connection) -> Result<Vec<ExportableFrameSet>> {
    let mut stmt = conn
        .prepare(
            "SELECT fs.id, fs.name, fs.total_exp_time,
                    (SELECT COUNT(*) FROM session_members sm
                     JOIN sessions s ON sm.session_id = s.id
                     JOIN imaging_nights i ON s.imaging_night_id = i.id
                     JOIN frames f ON sm.frame_id = f.id
                     WHERE i.frames_set_id = fs.id AND f.imagetyp = 'Light') as frame_count,
                    (SELECT f.object FROM session_members sm
                     JOIN sessions s ON sm.session_id = s.id
                     JOIN imaging_nights i ON s.imaging_night_id = i.id
                     JOIN frames f ON sm.frame_id = f.id
                     WHERE i.frames_set_id = fs.id AND f.object IS NOT NULL
                     LIMIT 1) as object_name,
                    (SELECT GROUP_CONCAT(DISTINCT f.filter) FROM session_members sm
                     JOIN sessions s ON sm.session_id = s.id
                     JOIN imaging_nights i ON s.imaging_night_id = i.id
                     JOIN frames f ON sm.frame_id = f.id
                     WHERE i.frames_set_id = fs.id AND f.filter IS NOT NULL) as filters
             FROM frames_set fs
             ORDER BY fs.id DESC",
        )?;

    let frame_sets = stmt
        .query_map([], |row| {
            let filters_str: Option<String> = row.get(5)?;
            let filters = filters_str
                .map(|s| s.split(',').map(|f| f.to_string()).collect())
                .unwrap_or_default();

            Ok(ExportableFrameSet {
                id: row.get(0)?,
                name: row.get(1)?,
                total_exposure_seconds: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                frame_count: row.get(3)?,
                object_name: row.get(4)?,
                filters,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(frame_sets)
}

/// Get calibration route for UI display
///
/// Returns a structured view of the export groups and their calibration trees,
/// suitable for displaying in the UI.
pub fn get_calibration_route(conn: &Connection, frame_set_id: i64) -> Result<CalibrationRoute> {
    // Collect export data
    let export_data = collect_export_data(conn, frame_set_id)?;

    // Build calibration route from export data
    let mut groups = Vec::new();
    let mut total_lights = 0;
    let mut total_exposure = 0.0;
    let mut unique_calibration_sets = std::collections::HashSet::new();
    let mut all_warnings = Vec::new();

    for group in &export_data.groups {
        let mut calibration_tree = Vec::new();

        // Build calibration tree for this group
        // Start with Light node
        let mut light_children = Vec::new();

        // Get calibration info from first subgroup
        if let Some(subgroup) = group.subgroups.first() {
            // Flat branch
            if let Some(ref flat) = subgroup.flat {
                unique_calibration_sets.insert(flat.set_id);
                let mut flat_children = Vec::new();

                // DarkFlat under Flat
                if let Some(ref dark_flat) = flat.dark_flat {
                    unique_calibration_sets.insert(dark_flat.set_id);
                    flat_children.push(CalibrationTreeNode {
                        node_type: "DarkFlat".to_string(),
                        label: format!("DarkFlat Set {} ({} frames)", dark_flat.set_id, dark_flat.frame_count),
                        set_id: Some(dark_flat.set_id),
                        count: dark_flat.frame_count,
                        children: vec![],
                        warnings: dark_flat.warnings.clone(),
                        is_missing: false,
                        is_shared: false,
                    });
                }

                // Dark under Flat (alternative to DarkFlat)
                if let Some(ref dark) = flat.dark {
                    unique_calibration_sets.insert(dark.set_id);
                    let mut dark_children = Vec::new();

                    if let Some(ref bias) = dark.bias {
                        unique_calibration_sets.insert(bias.set_id);
                        dark_children.push(CalibrationTreeNode {
                            node_type: "Bias".to_string(),
                            label: format!("Bias Set {} ({} frames)", bias.set_id, bias.frame_count),
                            set_id: Some(bias.set_id),
                            count: bias.frame_count,
                            children: vec![],
                            warnings: bias.warnings.clone(),
                            is_missing: false,
                            is_shared: false,
                        });
                    }

                    flat_children.push(CalibrationTreeNode {
                        node_type: "Dark".to_string(),
                        label: format!("Dark Set {} ({} frames)", dark.set_id, dark.frame_count),
                        set_id: Some(dark.set_id),
                        count: dark.frame_count,
                        children: dark_children,
                        warnings: dark.warnings.clone(),
                        is_missing: false,
                        is_shared: false,
                    });
                }

                // Bias under Flat
                if let Some(ref bias) = flat.bias {
                    unique_calibration_sets.insert(bias.set_id);
                    flat_children.push(CalibrationTreeNode {
                        node_type: "Bias".to_string(),
                        label: format!("Bias Set {} ({} frames)", bias.set_id, bias.frame_count),
                        set_id: Some(bias.set_id),
                        count: bias.frame_count,
                        children: vec![],
                        warnings: bias.warnings.clone(),
                        is_missing: false,
                        is_shared: false,
                    });
                }

                light_children.push(CalibrationTreeNode {
                    node_type: "Flat".to_string(),
                    label: format!("Flat Set {} ({} frames)", flat.set_id, flat.frame_count),
                    set_id: Some(flat.set_id),
                    count: flat.frame_count,
                    children: flat_children,
                    warnings: flat.warnings.clone(),
                    is_missing: false,
                    is_shared: false,
                });
            }

            // Dark branch (direct for lights)
            if let Some(ref dark) = subgroup.dark {
                unique_calibration_sets.insert(dark.set_id);
                let mut dark_children = Vec::new();

                if let Some(ref bias) = dark.bias {
                    unique_calibration_sets.insert(bias.set_id);
                    dark_children.push(CalibrationTreeNode {
                        node_type: "Bias".to_string(),
                        label: format!("Bias Set {} ({} frames)", bias.set_id, bias.frame_count),
                        set_id: Some(bias.set_id),
                        count: bias.frame_count,
                        children: vec![],
                        warnings: bias.warnings.clone(),
                        is_missing: false,
                        is_shared: false,
                    });
                }

                light_children.push(CalibrationTreeNode {
                    node_type: "Dark".to_string(),
                    label: format!("Dark Set {} ({} frames)", dark.set_id, dark.frame_count),
                    set_id: Some(dark.set_id),
                    count: dark.frame_count,
                    children: dark_children,
                    warnings: dark.warnings.clone(),
                    is_missing: false,
                    is_shared: false,
                });
            }

            // Bias branch (direct for lights)
            if let Some(ref bias) = subgroup.bias {
                unique_calibration_sets.insert(bias.set_id);
                light_children.push(CalibrationTreeNode {
                    node_type: "Bias".to_string(),
                    label: format!("Bias Set {} ({} frames)", bias.set_id, bias.frame_count),
                    set_id: Some(bias.set_id),
                    count: bias.frame_count,
                    children: vec![],
                    warnings: bias.warnings.clone(),
                    is_missing: false,
                    is_shared: false,
                });
            }

            all_warnings.extend(subgroup.warnings.clone());
        }

        // Add missing calibration warnings
        let has_flat = group.subgroups.first().and_then(|s| s.flat.as_ref()).is_some();
        let has_dark = group.subgroups.first().and_then(|s| s.dark.as_ref()).is_some();

        if !has_flat {
            light_children.push(CalibrationTreeNode {
                node_type: "Flat".to_string(),
                label: "No flat calibration".to_string(),
                set_id: None,
                count: 0,
                children: vec![],
                warnings: vec!["Missing flat calibration".to_string()],
                is_missing: true,
                is_shared: false,
            });
        }

        if !has_dark {
            light_children.push(CalibrationTreeNode {
                node_type: "Dark".to_string(),
                label: "No dark calibration".to_string(),
                set_id: None,
                count: 0,
                children: vec![],
                warnings: vec!["Missing dark calibration".to_string()],
                is_missing: true,
                is_shared: false,
            });
        }

        calibration_tree.push(CalibrationTreeNode {
            node_type: "Light".to_string(),
            label: format!("{} ({} frames)", group.display_name, group.total_frames),
            set_id: None,
            count: group.total_frames,
            children: light_children,
            warnings: group.warnings.clone(),
            is_missing: false,
            is_shared: false,
        });

        total_lights += group.total_frames;
        total_exposure += group.total_exposure;
        all_warnings.extend(group.warnings.clone());

        groups.push(CalibrationRouteGroup {
            name: group.display_name.clone(),
            light_count: group.total_frames,
            total_exposure: group.total_exposure,
            subgroup_count: group.subgroups.len() as i32,
            calibration_tree,
        });
    }

    // Build summary
    let summary = CalibrationRouteSummary {
        group_count: export_data.groups.len() as i32,
        total_lights,
        total_exposure,
        unique_calibration_sets: unique_calibration_sets.len() as i32,
        masters_to_create: export_data.master_plan.masters.len() as i32,
        flats_complete: export_data.calibration_summary.flats_complete,
        darks_complete: export_data.calibration_summary.darks_complete,
        bias_complete: export_data.calibration_summary.bias_complete,
        warnings: all_warnings,
    };

    Ok(CalibrationRoute {
        groups,
        summary,
    })
}

#[cfg(test)]
mod tests {    use super::*;
    use crate::db::schema::init_db;
    use rusqlite::params;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    /// Frame set + one imaging night + one session; returns `session_id`.
    fn seed_frame_set(conn: &Connection, fs_id: i64) -> i64 {
        conn.execute(
            "INSERT INTO frames_set (id, name, total_exp_time) VALUES (?1, ?2, 600.0)",
            params![fs_id, format!("Obj {fs_id}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
             VALUES (?1, '2026-07-05T20:00:00Z', '2026-07-05T23:00:00Z')",
            params![fs_id],
        )
        .unwrap();
        let night_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'TestCam')",
            params![night_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn seed_light(conn: &Connection, frame_id: i64, session_id: i64, filter: Option<&str>) {
        let file_id = frame_id + 2_000_000;
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
            params![
                file_id,
                format!("/test/light_{frame_id}.fits"),
                format!("light_{frame_id}.fits")
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, instrume, object, date_obs, filter, exptime)
             VALUES (?1, ?2, 'Light', 'TestCam', 'M31', '2026-07-05T20:30:00Z', ?3, 300.0)",
            params![frame_id, file_id, filter],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            params![session_id, frame_id],
        )
        .unwrap();
    }

    /// A raw (non-master) calibration set with `n` member frames.
    fn seed_raw_set(conn: &Connection, set_id: i64, imagetyp: &str, n: i64) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (?1, ?2, '2026-07-05', 0)",
            params![set_id, imagetyp],
        )
        .unwrap();
        for i in 0..n {
            let file_id = set_id * 100 + i + 5_000_000;
            let frame_id = set_id * 100 + i + 6_000_000;
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
                params![
                    file_id,
                    format!("/raw/{imagetyp}_{set_id}_{i}.fits"),
                    format!("{imagetyp}_{set_id}_{i}.fits")
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp) VALUES (?1, ?2, ?3)",
                params![frame_id, file_id, imagetyp],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                params![set_id, frame_id],
            )
            .unwrap();
        }
        set_id
    }

    fn add_link(conn: &Connection, frame_id: i64, set_id: i64, cal_type: &str) {
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (?1, 'frame', ?2, ?3, '2026-07-05T00:00:00Z')",
            params![frame_id, set_id, cal_type],
        )
        .unwrap();
    }

    #[test]
    fn exportable_frame_sets_summarizes_the_seeded_set() {
        let conn = mem();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 10, session, Some("Ha"));
        seed_light(&conn, 11, session, Some("Ha"));

        let sets = get_exportable_frame_sets(&conn).unwrap();

        assert_eq!(sets.len(), 1, "one seeded frame set");
        let fs = &sets[0];
        assert_eq!(fs.id, 1);
        assert_eq!(fs.name.as_deref(), Some("Obj 1"));
        assert_eq!(fs.total_exposure_seconds, 600.0);
        assert_eq!(fs.frame_count, 2, "lights counted via session_members");
        assert_eq!(fs.object_name.as_deref(), Some("M31"));
        assert_eq!(fs.filters, vec!["Ha".to_string()]);
    }

    #[test]
    fn calibration_route_root_carries_one_flat_child() {
        let conn = mem();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 10, session, Some("Ha"));
        let flat = seed_raw_set(&conn, 200, "Flat", 3);
        add_link(&conn, 10, flat, "Flat");

        let route = get_calibration_route(&conn, 1).unwrap();

        assert_eq!(route.groups.len(), 1, "one (filter, camera) export group");
        let group = &route.groups[0];
        assert_eq!(group.light_count, 1);
        assert_eq!(group.calibration_tree.len(), 1, "single Light root node");

        let root = &group.calibration_tree[0];
        assert_eq!(root.node_type, "Light");
        assert_eq!(root.count, 1);
        assert!(root.set_id.is_none(), "Light root has no calibration set id");

        let flats: Vec<_> = root
            .children
            .iter()
            .filter(|c| c.node_type == "Flat" && !c.is_missing)
            .collect();
        assert_eq!(flats.len(), 1, "exactly one Flat child, got {:?}", root.children);
        assert_eq!(flats[0].set_id, Some(flat));
        assert_eq!(flats[0].count, 3);
        assert_eq!(flats[0].label, "Flat Set 200 (3 frames)");
        assert!(flats[0].children.is_empty(), "no sub-calibrations linked");

        // The unlinked Dark still gets its missing-calibration placeholder.
        let missing_darks: Vec<_> = root
            .children
            .iter()
            .filter(|c| c.node_type == "Dark" && c.is_missing)
            .collect();
        assert_eq!(missing_darks.len(), 1, "missing-dark placeholder emitted");
        assert_eq!(missing_darks[0].label, "No dark calibration");

        assert_eq!(route.summary.group_count, 1);
        assert_eq!(route.summary.total_lights, 1);
        assert_eq!(
            route.summary.unique_calibration_sets, 1,
            "the linked flat set is the only unique calibration set"
        );
    }
}
