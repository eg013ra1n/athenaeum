// collab: catalog-side storage for Stage II collaboration projects (slice 3).
//
// Three tables (created in `db/schema.rs::init_db`), all owned here:
//
//   * `collab_projects` — poll cache, one row per project I'm a member of.
//     Refreshed wholesale on each hub poll; holds the RAW signed membership
//     snapshot (payload + signature, base64) so slice-4's project
//     `PeerAuthorizer` can re-verify offline without re-fetching.
//   * `project_links` — local project↔frame-set links. NEVER sent to the hub
//     (spec §7); the hub knows nothing about which of my sets back a project.
//   * `project_link_intents` — "publish as project" deep-link intents: when the
//     portal /new form was prefilled from a set, the next poll auto-links the
//     newly appeared project whose target matches (spec §8).

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

/// Column list shared by every read query, so `row_from_sql`'s index-based
/// `row.get(N)` calls can't silently drift out of sync with the SELECT.
const SELECT_COLS: &str = "project_id, slug, title, data_role, is_coordinator, require_approval, \
    pending_announcements, project_status, target_name, target_ra_deg, target_dec_deg, \
    target_radius_deg, membership_version, snapshot_payload_b64, snapshot_signature_b64, \
    members_json, thresholds_version, thresholds_rules_json, fetched_at";

/// One cached collaboration project (poll snapshot, refreshed wholesale).
#[derive(Debug, Clone, PartialEq)]
pub struct CollabProjectRow {
    pub project_id: String,
    pub slug: String,
    pub title: String,
    pub data_role: String,
    pub is_coordinator: bool,
    pub require_approval: bool,
    pub pending_announcements: i64,
    pub project_status: String,
    pub target_name: String,
    pub target_ra_deg: f64,
    pub target_dec_deg: f64,
    pub target_radius_deg: f64,
    pub membership_version: i64,
    /// RAW signed snapshot (base64) — payload + detached signature — kept so
    /// slice-4's project `PeerAuthorizer` can re-verify offline.
    pub snapshot_payload_b64: String,
    pub snapshot_signature_b64: String,
    pub members_json: String,
    pub thresholds_version: Option<i32>,
    pub thresholds_rules_json: Option<String>,
    /// Set by SQL (`datetime('now')`); ignored on write, populated on read.
    pub fetched_at: String,
}

fn row_from_sql(row: &rusqlite::Row) -> rusqlite::Result<CollabProjectRow> {
    Ok(CollabProjectRow {
        project_id: row.get(0)?,
        slug: row.get(1)?,
        title: row.get(2)?,
        data_role: row.get(3)?,
        is_coordinator: row.get::<_, i64>(4)? != 0,
        require_approval: row.get::<_, i64>(5)? != 0,
        pending_announcements: row.get(6)?,
        project_status: row.get(7)?,
        target_name: row.get(8)?,
        target_ra_deg: row.get(9)?,
        target_dec_deg: row.get(10)?,
        target_radius_deg: row.get(11)?,
        membership_version: row.get(12)?,
        snapshot_payload_b64: row.get(13)?,
        snapshot_signature_b64: row.get(14)?,
        members_json: row.get(15)?,
        thresholds_version: row.get(16)?,
        thresholds_rules_json: row.get(17)?,
        fetched_at: row.get(18)?,
    })
}

/// Insert or refresh the cache row for one project. Keyed on `project_id`; every
/// non-PK column is overwritten and `fetched_at` is stamped `datetime('now')`.
pub fn upsert_project(conn: &Connection, row: &CollabProjectRow) -> Result<()> {
    conn.execute(
        "INSERT INTO collab_projects
            (project_id, slug, title, data_role, is_coordinator, require_approval,
             pending_announcements, project_status, target_name, target_ra_deg, target_dec_deg,
             target_radius_deg, membership_version, snapshot_payload_b64, snapshot_signature_b64,
             members_json, thresholds_version, thresholds_rules_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
         ON CONFLICT(project_id) DO UPDATE SET
            slug = excluded.slug,
            title = excluded.title,
            data_role = excluded.data_role,
            is_coordinator = excluded.is_coordinator,
            require_approval = excluded.require_approval,
            pending_announcements = excluded.pending_announcements,
            project_status = excluded.project_status,
            target_name = excluded.target_name,
            target_ra_deg = excluded.target_ra_deg,
            target_dec_deg = excluded.target_dec_deg,
            target_radius_deg = excluded.target_radius_deg,
            membership_version = excluded.membership_version,
            snapshot_payload_b64 = excluded.snapshot_payload_b64,
            snapshot_signature_b64 = excluded.snapshot_signature_b64,
            members_json = excluded.members_json,
            thresholds_version = excluded.thresholds_version,
            thresholds_rules_json = excluded.thresholds_rules_json,
            fetched_at = datetime('now')",
        params![
            row.project_id,
            row.slug,
            row.title,
            row.data_role,
            row.is_coordinator as i64,
            row.require_approval as i64,
            row.pending_announcements,
            row.project_status,
            row.target_name,
            row.target_ra_deg,
            row.target_dec_deg,
            row.target_radius_deg,
            row.membership_version,
            row.snapshot_payload_b64,
            row.snapshot_signature_b64,
            row.members_json,
            row.thresholds_version,
            row.thresholds_rules_json,
        ],
    )?;
    Ok(())
}

/// All cached projects, ordered by title.
pub fn list_projects(conn: &Connection) -> Result<Vec<CollabProjectRow>> {
    let mut stmt =
        conn.prepare(&format!("SELECT {SELECT_COLS} FROM collab_projects ORDER BY title"))?;
    let rows = stmt
        .query_map([], row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// One cached project by id, if present.
pub fn get_project(conn: &Connection, project_id: &str) -> Result<Option<CollabProjectRow>> {
    conn.query_row(
        &format!("SELECT {SELECT_COLS} FROM collab_projects WHERE project_id = ?1"),
        params![project_id],
        row_from_sql,
    )
    .optional()
    .map_err(Into::into)
}

/// Delete every cache row whose `project_id` is NOT in `keep_ids` (the ids the
/// latest poll still returned). An empty list clears the whole cache. Returns
/// the number of rows removed.
pub fn prune_projects_not_in(conn: &Connection, keep_ids: &[String]) -> Result<usize> {
    if keep_ids.is_empty() {
        return Ok(conn.execute("DELETE FROM collab_projects", [])?);
    }
    let placeholders = vec!["?"; keep_ids.len()].join(", ");
    let sql = format!("DELETE FROM collab_projects WHERE project_id NOT IN ({placeholders})");
    let removed = conn.execute(&sql, rusqlite::params_from_iter(keep_ids.iter()))?;
    Ok(removed)
}

/// Link a frame set to a project locally (idempotent — a repeated link is a
/// no-op). NEVER sent to the hub.
pub fn link_set(conn: &Connection, project_id: &str, frames_set_id: i64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO project_links (project_id, frames_set_id) VALUES (?1, ?2)",
        params![project_id, frames_set_id],
    )?;
    Ok(())
}

/// Remove a project↔frame-set link. Returns the number of rows removed (0 when
/// the link was already gone, e.g. cascaded away by a set delete).
pub fn unlink_set(conn: &Connection, project_id: &str, frames_set_id: i64) -> Result<usize> {
    let removed = conn.execute(
        "DELETE FROM project_links WHERE project_id = ?1 AND frames_set_id = ?2",
        params![project_id, frames_set_id],
    )?;
    Ok(removed)
}

/// The frame-set ids linked to a project, ascending.
pub fn linked_set_ids(conn: &Connection, project_id: &str) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT frames_set_id FROM project_links WHERE project_id = ?1 ORDER BY frames_set_id",
    )?;
    let ids = stmt
        .query_map(params![project_id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

/// Whether a given frame set is linked to a project.
pub fn is_set_linked(conn: &Connection, project_id: &str, frames_set_id: i64) -> Result<bool> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_links WHERE project_id = ?1 AND frames_set_id = ?2)",
        params![project_id, frames_set_id],
        |r| r.get(0),
    )?;
    Ok(exists)
}

/// Record a "publish as project" intent for a set (its target RA/Dec captured at
/// prefill time). The next poll matches a newly appeared project's target
/// against this and auto-links the source set. Returns the new intent id.
pub fn add_link_intent(
    conn: &Connection,
    frames_set_id: i64,
    ra_deg: f64,
    dec_deg: f64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO project_link_intents (frames_set_id, ra_deg, dec_deg) VALUES (?1, ?2, ?3)",
        params![frames_set_id, ra_deg, dec_deg],
    )?;
    Ok(conn.last_insert_rowid())
}

/// All pending link intents as `(intent_id, frames_set_id, ra_deg, dec_deg)`,
/// oldest first.
pub fn list_link_intents(conn: &Connection) -> Result<Vec<(i64, i64, f64, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT id, frames_set_id, ra_deg, dec_deg FROM project_link_intents ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Delete a link intent once it's been consumed (or abandoned).
pub fn delete_link_intent(conn: &Connection, intent_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM project_link_intents WHERE id = ?1",
        params![intent_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn
    }

    fn sample_row(id: &str) -> CollabProjectRow {
        CollabProjectRow {
            project_id: id.to_string(),
            slug: format!("{id}-slug"),
            title: format!("Project {id}"),
            data_role: "send_receive".into(),
            is_coordinator: true,
            require_approval: false,
            pending_announcements: 0,
            project_status: "active".into(),
            target_name: "M101".into(),
            target_ra_deg: 210.8,
            target_dec_deg: 54.35,
            target_radius_deg: 1.5,
            membership_version: 1,
            snapshot_payload_b64: "cGF5bG9hZA==".into(),
            snapshot_signature_b64: "c2ln".into(),
            members_json: "[]".into(),
            thresholds_version: Some(1),
            thresholds_rules_json: Some("[]".into()),
            fetched_at: String::new(), // set by SQL
        }
    }

    #[test]
    fn cache_upsert_list_prune_roundtrip() {
        let conn = test_conn();
        upsert_project(&conn, &sample_row("p-1")).unwrap();
        upsert_project(&conn, &sample_row("p-2")).unwrap();

        // Upsert updates in place (no duplicate rows).
        let mut updated = sample_row("p-1");
        updated.title = "Renamed".into();
        updated.membership_version = 5;
        upsert_project(&conn, &updated).unwrap();

        let all = list_projects(&conn).unwrap();
        assert_eq!(all.len(), 2);
        let p1 = get_project(&conn, "p-1").unwrap().unwrap();
        assert_eq!(p1.title, "Renamed");
        assert_eq!(p1.membership_version, 5);
        assert!(!p1.fetched_at.is_empty());

        // Prune keeps only the listed ids.
        let removed = prune_projects_not_in(&conn, &["p-2".to_string()]).unwrap();
        assert_eq!(removed, 1);
        assert!(get_project(&conn, "p-1").unwrap().is_none());
    }

    #[test]
    fn links_and_intents_respect_fk_cascade() {
        let conn = test_conn();
        // A real frames_set row for the FK.
        conn.execute("INSERT INTO frames_set (name) VALUES ('S1')", []).unwrap();
        let set_id = conn.last_insert_rowid();

        link_set(&conn, "p-1", set_id).unwrap();
        link_set(&conn, "p-1", set_id).unwrap(); // idempotent
        assert!(is_set_linked(&conn, "p-1", set_id).unwrap());
        assert_eq!(linked_set_ids(&conn, "p-1").unwrap(), vec![set_id]);

        let intent = add_link_intent(&conn, set_id, 210.8, 54.35).unwrap();
        assert_eq!(list_link_intents(&conn).unwrap().len(), 1);
        delete_link_intent(&conn, intent).unwrap();
        assert!(list_link_intents(&conn).unwrap().is_empty());

        // Deleting the set cascades the link away.
        add_link_intent(&conn, set_id, 1.0, 2.0).unwrap();
        conn.execute("DELETE FROM frames_set WHERE id = ?1", [set_id]).unwrap();
        assert!(linked_set_ids(&conn, "p-1").unwrap().is_empty());
        assert!(list_link_intents(&conn).unwrap().is_empty());

        assert_eq!(unlink_set(&conn, "p-1", set_id).unwrap(), 0, "already gone");
    }
}
