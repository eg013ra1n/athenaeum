// collab_exchange: catalog-side storage for the Stage II collaboration package
// exchange (slice 4).
//
// Two tables (created in `db/schema.rs::init_db`), owned here:
//
//   * `project_packages` — every package I know about, whether I published it
//     (`origin='mine'`), received it (`origin='received'`), or merely saw it in
//     the hub list (`origin='remote'`). The row mirrors the hub-anchored
//     decision `state` (pending|published|rejected) plus swarm counts captured
//     at poll time (`holder_count`/`online_count`), alongside local-only
//     progress (`local_status`, `local_dir`, `manifest_ndjson`). The retained
//     `manifest_ndjson` bytes (mine + fully-received) let me re-serve a package
//     byte-identically (Д2) without re-deriving the manifest.
//   * `project_contributions` — received frames. These NEVER enter
//     `files`/`frames`; they land under a managed per-project root and are
//     tracked here only. Cascades away with its parent package.
//
// House idiom (mirrors `db/collab.rs`): a `*_SELECT_COLS` const paired with an
// index-based row mapper so `row.get(N)` can't drift out of sync with the
// SELECT; `anyhow::Result`, `params!`, `OptionalExtension`.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

// ---------------------------------------------------------------------------
// project_packages
// ---------------------------------------------------------------------------

/// Column list shared by every `project_packages` read query. Order is pinned to
/// `package_row_from_sql`'s index-based `row.get(N)` calls.
const PACKAGE_SELECT_COLS: &str = "package_id, project_id, announcement_id, publisher_display, own, \
    root_hash, byte_size, frame_count, manifest_xxh3, aggregate_stats, supersedes, state, \
    reject_reason, superseded, origin, local_dir, manifest_ndjson, local_status, holder_count, \
    online_count, created_at, decided_at, fetched_at";

/// One known collaboration package (mine / received / remote).
#[derive(Debug, Clone, PartialEq)]
pub struct PackageRow {
    pub package_id: String,
    pub project_id: String,
    pub announcement_id: String,
    pub publisher_display: String,
    /// I published this package.
    pub own: bool,
    pub root_hash: String,
    pub byte_size: i64,
    pub frame_count: i64,
    /// Hub-anchored manifest hash (`aggregateStats`); NULL when the publisher
    /// pre-dates Д2.
    pub manifest_xxh3: Option<String>,
    /// JSON blob of hub-anchored aggregate stats.
    pub aggregate_stats: String,
    /// JSON array of announcement ids this package supersedes.
    pub supersedes: String,
    /// Hub-mirrored decision: `pending` | `published` | `rejected`.
    pub state: String,
    pub reject_reason: Option<String>,
    /// Set when another announcement supersedes this one.
    pub superseded: bool,
    /// `mine` | `received` | `remote`.
    pub origin: String,
    /// Retained publish dir (origin='mine').
    pub local_dir: Option<String>,
    /// Exact manifest bytes (mine + fully-received) for byte-identical re-serving (Д2).
    pub manifest_ndjson: Option<Vec<u8>>,
    /// Local fetch progress: `none` | `downloading` | `complete` | `failed`.
    pub local_status: String,
    /// Holders captured from the hub list at poll time (Task 8 writes, Task 11 reads).
    pub holder_count: i64,
    pub online_count: i64,
    pub created_at: String,
    pub decided_at: Option<String>,
    /// Set by SQL (`datetime('now')`); ignored on write, populated on read.
    pub fetched_at: String,
}

fn package_row_from_sql(row: &rusqlite::Row) -> rusqlite::Result<PackageRow> {
    Ok(PackageRow {
        package_id: row.get(0)?,
        project_id: row.get(1)?,
        announcement_id: row.get(2)?,
        publisher_display: row.get(3)?,
        own: row.get::<_, i64>(4)? != 0,
        root_hash: row.get(5)?,
        byte_size: row.get(6)?,
        frame_count: row.get(7)?,
        manifest_xxh3: row.get(8)?,
        aggregate_stats: row.get(9)?,
        supersedes: row.get(10)?,
        state: row.get(11)?,
        reject_reason: row.get(12)?,
        superseded: row.get::<_, i64>(13)? != 0,
        origin: row.get(14)?,
        local_dir: row.get(15)?,
        manifest_ndjson: row.get(16)?,
        local_status: row.get(17)?,
        holder_count: row.get(18)?,
        online_count: row.get(19)?,
        created_at: row.get(20)?,
        decided_at: row.get(21)?,
        fetched_at: row.get(22)?,
    })
}

/// Insert or refresh a package row, keyed on `package_id`.
///
/// On INSERT every column comes from `row`. On CONFLICT the hub-anchored and
/// announcement columns are overwritten from `row` and `fetched_at` is re-stamped,
/// but three classes of column are protected so a hub poll (which knows nothing
/// about my local progress) can't clobber it:
///
///   * `local_status` is **preserved** — it only ever changes via
///     [`set_local_status`].
///   * `manifest_ndjson` and `local_dir` are preserved unless `row` supplies a
///     non-NULL value (`COALESCE(excluded, existing)`), so a poll passing NULL
///     keeps the retained bytes while a re-publish/complete can still set them.
///   * `origin` and `own` only move **forward** (`remote → received → mine`,
///     `0 → 1`); a later poll can never downgrade a received/owned package back
///     to remote.
pub fn upsert_package(conn: &Connection, row: &PackageRow) -> Result<()> {
    conn.execute(
        "INSERT INTO project_packages
            (package_id, project_id, announcement_id, publisher_display, own, root_hash,
             byte_size, frame_count, manifest_xxh3, aggregate_stats, supersedes, state,
             reject_reason, superseded, origin, local_dir, manifest_ndjson, local_status,
             holder_count, online_count, created_at, decided_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                 ?18, ?19, ?20, ?21, ?22)
         ON CONFLICT(package_id) DO UPDATE SET
            project_id        = excluded.project_id,
            announcement_id   = excluded.announcement_id,
            publisher_display = excluded.publisher_display,
            own               = MAX(own, excluded.own),
            root_hash         = excluded.root_hash,
            byte_size         = excluded.byte_size,
            frame_count       = excluded.frame_count,
            manifest_xxh3     = excluded.manifest_xxh3,
            aggregate_stats   = excluded.aggregate_stats,
            supersedes        = excluded.supersedes,
            state             = excluded.state,
            reject_reason     = excluded.reject_reason,
            superseded        = excluded.superseded,
            origin            = CASE origin
                                    WHEN 'mine' THEN 'mine'
                                    WHEN 'received' THEN
                                        CASE WHEN excluded.origin = 'mine' THEN 'mine'
                                             ELSE 'received' END
                                    ELSE excluded.origin
                                END,
            local_dir         = COALESCE(excluded.local_dir, local_dir),
            manifest_ndjson   = COALESCE(excluded.manifest_ndjson, manifest_ndjson),
            holder_count      = excluded.holder_count,
            online_count      = excluded.online_count,
            created_at        = excluded.created_at,
            decided_at        = excluded.decided_at,
            fetched_at        = datetime('now')",
        params![
            row.package_id,
            row.project_id,
            row.announcement_id,
            row.publisher_display,
            row.own as i64,
            row.root_hash,
            row.byte_size,
            row.frame_count,
            row.manifest_xxh3,
            row.aggregate_stats,
            row.supersedes,
            row.state,
            row.reject_reason,
            row.superseded as i64,
            row.origin,
            row.local_dir,
            row.manifest_ndjson,
            row.local_status,
            row.holder_count,
            row.online_count,
            row.created_at,
            row.decided_at,
        ],
    )?;
    Ok(())
}

/// One package by id, if present.
pub fn get_package(conn: &Connection, package_id: &str) -> Result<Option<PackageRow>> {
    conn.query_row(
        &format!("SELECT {PACKAGE_SELECT_COLS} FROM project_packages WHERE package_id = ?1"),
        params![package_id],
        package_row_from_sql,
    )
    .optional()
    .map_err(Into::into)
}

/// One package by its (unique) announcement id, if present.
pub fn get_package_by_announcement(
    conn: &Connection,
    announcement_id: &str,
) -> Result<Option<PackageRow>> {
    conn.query_row(
        &format!("SELECT {PACKAGE_SELECT_COLS} FROM project_packages WHERE announcement_id = ?1"),
        params![announcement_id],
        package_row_from_sql,
    )
    .optional()
    .map_err(Into::into)
}

/// Every known package for a project, newest announcement first.
pub fn list_packages(conn: &Connection, project_id: &str) -> Result<Vec<PackageRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {PACKAGE_SELECT_COLS} FROM project_packages WHERE project_id = ?1 \
         ORDER BY created_at DESC, package_id DESC"
    ))?;
    let rows = stmt
        .query_map(params![project_id], package_row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Update local fetch progress for a package (`none`|`downloading`|`complete`|`failed`).
/// The only path that mutates `local_status` (upsert preserves it).
pub fn set_local_status(conn: &Connection, package_id: &str, status: &str) -> Result<()> {
    conn.execute(
        "UPDATE project_packages SET local_status = ?2 WHERE package_id = ?1",
        params![package_id, status],
    )?;
    Ok(())
}

/// Store the exact manifest bytes for a package (mine + fully-received), enabling
/// byte-identical re-serving (Д2).
pub fn set_manifest(conn: &Connection, package_id: &str, bytes: &[u8]) -> Result<()> {
    conn.execute(
        "UPDATE project_packages SET manifest_ndjson = ?2 WHERE package_id = ?1",
        params![package_id, bytes],
    )?;
    Ok(())
}

/// Flag every listed announcement as superseded. Returns the number of rows
/// updated. An empty slice is a no-op.
pub fn mark_superseded(conn: &Connection, announcement_ids: &[String]) -> Result<usize> {
    if announcement_ids.is_empty() {
        return Ok(0);
    }
    let placeholders = vec!["?"; announcement_ids.len()].join(", ");
    let sql = format!(
        "UPDATE project_packages SET superseded = 1 WHERE announcement_id IN ({placeholders})"
    );
    let updated = conn.execute(&sql, rusqlite::params_from_iter(announcement_ids.iter()))?;
    Ok(updated)
}

/// Delete a package (and, via `ON DELETE CASCADE`, its contributions — requires
/// `PRAGMA foreign_keys = ON`). Returns the number of package rows removed (0/1).
pub fn delete_package(conn: &Connection, package_id: &str) -> Result<usize> {
    let removed = conn.execute(
        "DELETE FROM project_packages WHERE package_id = ?1",
        params![package_id],
    )?;
    Ok(removed)
}

// ---------------------------------------------------------------------------
// project_contributions
// ---------------------------------------------------------------------------

/// Column list shared by every `project_contributions` read query. Order is
/// pinned to `contribution_row_from_sql`'s index-based `row.get(N)` calls.
const CONTRIBUTION_SELECT_COLS: &str = "id, project_id, package_id, frame_uuid, publisher_display, \
    rel_path, landed_path, byte_size, xxh3, frame_meta, analysis, superseded, created_at";

/// One received frame (never enters `files`/`frames`).
#[derive(Debug, Clone, PartialEq)]
pub struct ContributionRow {
    /// Set by SQL on insert; ignored on write.
    pub id: i64,
    pub project_id: String,
    pub package_id: String,
    pub frame_uuid: String,
    pub publisher_display: String,
    pub rel_path: String,
    /// Absolute on-disk path where the frame landed (UNIQUE).
    pub landed_path: String,
    pub byte_size: i64,
    /// Full-content, manifest-anchored hash.
    pub xxh3: String,
    /// JSON blob of copied-through frame metadata.
    pub frame_meta: String,
    pub analysis: Option<String>,
    pub superseded: bool,
    /// Set by SQL (`datetime('now')`); ignored on write, populated on read.
    pub created_at: String,
}

fn contribution_row_from_sql(row: &rusqlite::Row) -> rusqlite::Result<ContributionRow> {
    Ok(ContributionRow {
        id: row.get(0)?,
        project_id: row.get(1)?,
        package_id: row.get(2)?,
        frame_uuid: row.get(3)?,
        publisher_display: row.get(4)?,
        rel_path: row.get(5)?,
        landed_path: row.get(6)?,
        byte_size: row.get(7)?,
        xxh3: row.get(8)?,
        frame_meta: row.get(9)?,
        analysis: row.get(10)?,
        superseded: row.get::<_, i64>(11)? != 0,
        created_at: row.get(12)?,
    })
}

/// Insert a received-frame row. Returns its new autoincrement id. `created_at`
/// is left to the column default when the caller passes an empty string.
pub fn insert_contribution(conn: &Connection, row: &ContributionRow) -> Result<i64> {
    if row.created_at.is_empty() {
        conn.execute(
            "INSERT INTO project_contributions
                (project_id, package_id, frame_uuid, publisher_display, rel_path, landed_path,
                 byte_size, xxh3, frame_meta, analysis, superseded)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                row.project_id,
                row.package_id,
                row.frame_uuid,
                row.publisher_display,
                row.rel_path,
                row.landed_path,
                row.byte_size,
                row.xxh3,
                row.frame_meta,
                row.analysis,
                row.superseded as i64,
            ],
        )?;
    } else {
        conn.execute(
            "INSERT INTO project_contributions
                (project_id, package_id, frame_uuid, publisher_display, rel_path, landed_path,
                 byte_size, xxh3, frame_meta, analysis, superseded, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                row.project_id,
                row.package_id,
                row.frame_uuid,
                row.publisher_display,
                row.rel_path,
                row.landed_path,
                row.byte_size,
                row.xxh3,
                row.frame_meta,
                row.analysis,
                row.superseded as i64,
                row.created_at,
            ],
        )?;
    }
    Ok(conn.last_insert_rowid())
}

/// Every contribution belonging to a package, oldest first.
pub fn contributions_for_package(
    conn: &Connection,
    package_id: &str,
) -> Result<Vec<ContributionRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CONTRIBUTION_SELECT_COLS} FROM project_contributions \
         WHERE package_id = ?1 ORDER BY id"
    ))?;
    let rows = stmt
        .query_map(params![package_id], contribution_row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Every contribution for a project, oldest first.
pub fn contributions_for_project(
    conn: &Connection,
    project_id: &str,
) -> Result<Vec<ContributionRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CONTRIBUTION_SELECT_COLS} FROM project_contributions \
         WHERE project_id = ?1 ORDER BY id"
    ))?;
    let rows = stmt
        .query_map(params![project_id], contribution_row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// uuid-supersede: a publisher re-published the frame carrying `frame_uuid` with
/// new content, so the older contribution row for that (project, publisher, uuid)
/// must go. Deletes it and returns its `landed_path` so the caller can remove the
/// stale file; `None` when there was no prior row.
pub fn replace_contribution_for_uuid(
    conn: &Connection,
    project_id: &str,
    publisher: &str,
    frame_uuid: &str,
) -> Result<Option<String>> {
    let old_path: Option<String> = conn
        .query_row(
            "SELECT landed_path FROM project_contributions \
             WHERE project_id = ?1 AND publisher_display = ?2 AND frame_uuid = ?3 \
             ORDER BY id LIMIT 1",
            params![project_id, publisher, frame_uuid],
            |r| r.get(0),
        )
        .optional()?;
    if old_path.is_some() {
        conn.execute(
            "DELETE FROM project_contributions \
             WHERE project_id = ?1 AND publisher_display = ?2 AND frame_uuid = ?3",
            params![project_id, publisher, frame_uuid],
        )?;
    }
    Ok(old_path)
}

/// One contribution by its (unique) `landed_path`, if present. The scanner's
/// project-contribution reconcile uses this for the **known** branch — a stamped
/// file scanned at the exact path a tracking row already records is a no-op.
pub fn find_contribution_by_landed_path(
    conn: &Connection,
    landed_path: &str,
) -> Result<Option<ContributionRow>> {
    conn.query_row(
        &format!(
            "SELECT {CONTRIBUTION_SELECT_COLS} FROM project_contributions WHERE landed_path = ?1"
        ),
        params![landed_path],
        contribution_row_from_sql,
    )
    .optional()
    .map_err(Into::into)
}

/// One contribution matching `(project_id, xxh3)` — the full-content hash the
/// manifest anchors (`package::xxh3_full_file`), scoped to the project so a copy
/// of the same bytes under a different project can't cross-match. The scanner's
/// reconcile uses this for the **moved**/**duplicate** branches; oldest row wins
/// on the (rare) intra-project content collision.
pub fn find_contribution_by_project_and_hash(
    conn: &Connection,
    project_id: &str,
    xxh3: &str,
) -> Result<Option<ContributionRow>> {
    conn.query_row(
        &format!(
            "SELECT {CONTRIBUTION_SELECT_COLS} FROM project_contributions \
             WHERE project_id = ?1 AND xxh3 = ?2 ORDER BY id LIMIT 1"
        ),
        params![project_id, xxh3],
        contribution_row_from_sql,
    )
    .optional()
    .map_err(Into::into)
}

/// Repoint a contribution's `landed_path` to a new on-disk location (the
/// **moved** branch of the scanner reconcile). `landed_path` is UNIQUE; the
/// caller has already established that no row occupies `new_path`.
pub fn update_contribution_landed_path(conn: &Connection, id: i64, new_path: &str) -> Result<()> {
    conn.execute(
        "UPDATE project_contributions SET landed_path = ?2 WHERE id = ?1",
        params![id, new_path],
    )?;
    Ok(())
}

/// Д9 supersede computation. When I publish a new package touching `uuids`, this
/// returns the announcement ids of MY still-active packages (`own = 1`,
/// `state != 'rejected'`, not already superseded) whose retained manifest
/// contains any of those source-frame uuids — the announcements the new one
/// should supersede.
///
/// "Contains" is a substring test against the retained `manifest_ndjson` bytes:
/// v4 uuids are unique 36-char strings, so a manifest that lists a frame's uuid
/// (whatever the exact JSON field name) matches, and the check stays agnostic to
/// the manifest's serialization schema. Packages without retained manifest bytes
/// contribute nothing.
pub fn own_active_announcement_ids_for_uuids(
    conn: &Connection,
    project_id: &str,
    uuids: &[String],
) -> Result<Vec<String>> {
    // A v4 uuid is 36 chars: ignore anything shorter than 32 so an empty/short
    // string can't substring-match every retained manifest and mass-supersede
    // unrelated packages (F5).
    let uuids: Vec<&str> = uuids
        .iter()
        .map(String::as_str)
        .filter(|u| u.len() >= 32)
        .collect();
    if uuids.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT announcement_id, manifest_ndjson FROM project_packages \
         WHERE project_id = ?1 AND own = 1 AND state != 'rejected' AND superseded = 0",
    )?;
    let rows = stmt
        .query_map(params![project_id], |r| {
            let announcement_id: String = r.get(0)?;
            let manifest: Option<Vec<u8>> = r.get(1)?;
            Ok((announcement_id, manifest))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut out = Vec::new();
    for (announcement_id, manifest) in rows {
        let Some(bytes) = manifest else { continue };
        let text = String::from_utf8_lossy(&bytes);
        if uuids.iter().any(|u| text.contains(*u)) {
            out.push(announcement_id);
        }
    }
    Ok(out)
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

    fn sample_package(id: &str, project: &str, created_at: &str) -> PackageRow {
        PackageRow {
            package_id: id.to_string(),
            project_id: project.to_string(),
            announcement_id: format!("ann-{id}"),
            publisher_display: "Alice".into(),
            own: false,
            root_hash: "roothash".into(),
            byte_size: 4096,
            frame_count: 3,
            manifest_xxh3: Some("mx".into()),
            aggregate_stats: "{}".into(),
            supersedes: "[]".into(),
            state: "pending".into(),
            reject_reason: None,
            superseded: false,
            origin: "remote".into(),
            local_dir: None,
            manifest_ndjson: None,
            local_status: "none".into(),
            holder_count: 0,
            online_count: 0,
            created_at: created_at.to_string(),
            decided_at: None,
            fetched_at: String::new(),
        }
    }

    fn sample_contribution(project: &str, package_id: &str, uuid: &str, landed: &str) -> ContributionRow {
        ContributionRow {
            id: 0,
            project_id: project.to_string(),
            package_id: package_id.to_string(),
            frame_uuid: uuid.to_string(),
            publisher_display: "Alice".into(),
            rel_path: format!("Alice/{uuid}.fits"),
            landed_path: landed.to_string(),
            byte_size: 2048,
            xxh3: "contenthash".into(),
            frame_meta: "{}".into(),
            analysis: None,
            superseded: false,
            created_at: String::new(),
        }
    }

    #[test]
    fn package_upsert_roundtrip_and_list_order() {
        let conn = test_conn();

        // Two packages in the same project, p2 announced later than p1.
        let mut p1 = sample_package("p1", "proj-1", "2026-07-10 00:00:00");
        p1.manifest_xxh3 = Some("hash1".into());
        p1.holder_count = 2;
        p1.online_count = 1;
        p1.decided_at = Some("2026-07-11 00:00:00".into());
        p1.state = "published".into();
        upsert_package(&conn, &p1).unwrap();
        upsert_package(&conn, &sample_package("p2", "proj-1", "2026-07-12 00:00:00")).unwrap();
        // A package in another project must not leak into the list.
        upsert_package(&conn, &sample_package("p3", "proj-2", "2026-07-13 00:00:00")).unwrap();

        // Newest announcement first.
        let listed = list_packages(&conn, "proj-1").unwrap();
        assert_eq!(
            listed.iter().map(|p| p.package_id.as_str()).collect::<Vec<_>>(),
            vec!["p2", "p1"]
        );

        // Full roundtrip on p1 (fetched_at is SQL-set, everything else preserved).
        let got = get_package(&conn, "p1").unwrap().unwrap();
        assert!(!got.fetched_at.is_empty());
        let mut expect = p1.clone();
        expect.fetched_at = got.fetched_at.clone();
        assert_eq!(got, expect);

        // Lookup by announcement id.
        let by_ann = get_package_by_announcement(&conn, "ann-p1").unwrap().unwrap();
        assert_eq!(by_ann.package_id, "p1");

        // Upsert is in place (no duplicate row) and refreshes hub-mirrored fields.
        let mut p1b = sample_package("p1", "proj-1", "2026-07-10 00:00:00");
        p1b.state = "rejected".into();
        p1b.reject_reason = Some("dupe".into());
        p1b.holder_count = 9;
        upsert_package(&conn, &p1b).unwrap();
        assert_eq!(list_packages(&conn, "proj-1").unwrap().len(), 2);
        let got = get_package(&conn, "p1").unwrap().unwrap();
        assert_eq!(got.state, "rejected");
        assert_eq!(got.reject_reason.as_deref(), Some("dupe"));
        assert_eq!(got.holder_count, 9);
    }

    #[test]
    fn upsert_preserves_local_progress_and_monotonic_origin() {
        let conn = test_conn();
        upsert_package(&conn, &sample_package("p1", "proj-1", "2026-07-10 00:00:00")).unwrap();

        // Local progress set via the dedicated setters.
        set_local_status(&conn, "p1", "downloading").unwrap();
        set_manifest(&conn, "p1", b"{\"uuid\":\"u-1\"}\n").unwrap();

        // A poll upsert (origin='remote', local_status='none', manifest NULL) must
        // not clobber local progress, and origin only moves forward.
        let mut poll = sample_package("p1", "proj-1", "2026-07-10 00:00:00");
        poll.origin = "remote".into();
        poll.local_status = "none".into();
        poll.manifest_ndjson = None;
        upsert_package(&conn, &poll).unwrap();

        let got = get_package(&conn, "p1").unwrap().unwrap();
        assert_eq!(got.local_status, "downloading", "local_status preserved");
        assert_eq!(
            got.manifest_ndjson.as_deref(),
            Some(&b"{\"uuid\":\"u-1\"}\n"[..]),
            "manifest bytes preserved"
        );

        // A download-complete read-modify-write upgrades origin remote -> received.
        let mut recv = got.clone();
        recv.origin = "received".into();
        recv.fetched_at = String::new();
        upsert_package(&conn, &recv).unwrap();
        assert_eq!(get_package(&conn, "p1").unwrap().unwrap().origin, "received");

        // A later poll (origin='remote') must NOT downgrade it back.
        let mut poll2 = sample_package("p1", "proj-1", "2026-07-10 00:00:00");
        poll2.origin = "remote".into();
        upsert_package(&conn, &poll2).unwrap();
        assert_eq!(get_package(&conn, "p1").unwrap().unwrap().origin, "received");
    }

    #[test]
    fn contribution_insert_and_cascade() {
        let conn = test_conn();
        upsert_package(&conn, &sample_package("p1", "proj-1", "2026-07-10 00:00:00")).unwrap();

        let id1 = insert_contribution(
            &conn,
            &sample_contribution("proj-1", "p1", "u-1", "/land/a.fits"),
        )
        .unwrap();
        let id2 = insert_contribution(
            &conn,
            &sample_contribution("proj-1", "p1", "u-2", "/land/b.fits"),
        )
        .unwrap();
        assert!(id2 > id1);

        assert_eq!(contributions_for_package(&conn, "p1").unwrap().len(), 2);
        assert_eq!(contributions_for_project(&conn, "proj-1").unwrap().len(), 2);
        let one = &contributions_for_package(&conn, "p1").unwrap()[0];
        assert!(!one.created_at.is_empty(), "created_at defaulted by SQL");

        // Deleting the package cascades its contributions away.
        assert_eq!(delete_package(&conn, "p1").unwrap(), 1);
        assert!(get_package(&conn, "p1").unwrap().is_none());
        assert!(contributions_for_project(&conn, "proj-1").unwrap().is_empty());
    }

    #[test]
    fn contribution_lookup_by_landed_path_and_hash() {
        let conn = test_conn();
        upsert_package(&conn, &sample_package("p1", "proj-1", "2026-07-10 00:00:00")).unwrap();
        // A second project (shares the same content hash) to prove scoping.
        upsert_package(&conn, &sample_package("p2", "proj-2", "2026-07-10 00:00:00")).unwrap();

        let mut c = sample_contribution("proj-1", "p1", "u-1", "/land/a.fits");
        c.xxh3 = "deadbeefdeadbeef".into();
        let id = insert_contribution(&conn, &c).unwrap();
        let mut other = sample_contribution("proj-2", "p2", "u-9", "/land/other.fits");
        other.xxh3 = "deadbeefdeadbeef".into(); // same bytes, different project
        insert_contribution(&conn, &other).unwrap();

        // by landed_path: exact hit / miss.
        assert_eq!(
            find_contribution_by_landed_path(&conn, "/land/a.fits").unwrap().unwrap().id,
            id
        );
        assert!(find_contribution_by_landed_path(&conn, "/land/nope.fits").unwrap().is_none());

        // by (project, hash): scoped to the project — never cross-matches proj-2.
        let hit = find_contribution_by_project_and_hash(&conn, "proj-1", "deadbeefdeadbeef")
            .unwrap()
            .unwrap();
        assert_eq!(hit.id, id);
        assert!(find_contribution_by_project_and_hash(&conn, "proj-1", "0000000000000000")
            .unwrap()
            .is_none());
        // The hash exists but only under proj-2 -> a proj-3 query misses.
        assert!(find_contribution_by_project_and_hash(&conn, "proj-3", "deadbeefdeadbeef")
            .unwrap()
            .is_none());

        // Repoint (moved branch) and confirm the new path resolves, old does not.
        update_contribution_landed_path(&conn, id, "/land/moved.fits").unwrap();
        assert_eq!(
            find_contribution_by_landed_path(&conn, "/land/moved.fits").unwrap().unwrap().id,
            id
        );
        assert!(find_contribution_by_landed_path(&conn, "/land/a.fits").unwrap().is_none());
    }

    #[test]
    fn replace_contribution_for_uuid_removes_and_returns_path() {
        let conn = test_conn();
        upsert_package(&conn, &sample_package("p1", "proj-1", "2026-07-10 00:00:00")).unwrap();
        insert_contribution(
            &conn,
            &sample_contribution("proj-1", "p1", "u-1", "/land/old.fits"),
        )
        .unwrap();
        // A different publisher's frame for the same uuid must be untouched.
        let mut other = sample_contribution("proj-1", "p1", "u-1", "/land/bob.fits");
        other.publisher_display = "Bob".into();
        insert_contribution(&conn, &other).unwrap();

        let old = replace_contribution_for_uuid(&conn, "proj-1", "Alice", "u-1").unwrap();
        assert_eq!(old.as_deref(), Some("/land/old.fits"));
        // Alice's row gone, Bob's kept.
        let remaining = contributions_for_project(&conn, "proj-1").unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].publisher_display, "Bob");

        // Second call finds nothing.
        assert_eq!(
            replace_contribution_for_uuid(&conn, "proj-1", "Alice", "u-1").unwrap(),
            None
        );
    }

    #[test]
    fn own_active_announcement_ids_for_uuids_filters() {
        let conn = test_conn();

        // Realistic v4 uuids (36 chars) — the substring match keys on full uuids,
        // and the F5 length filter drops anything shorter than 32.
        let u1 = "11111111-1111-4111-8111-111111111111";
        let u9 = "99999999-9999-4999-8999-999999999999";
        let u_none = "22222222-2222-4222-8222-222222222222";

        // own + active + manifest contains u1  -> included
        let mut own_active = sample_package("p-own", "proj-1", "2026-07-10 00:00:00");
        own_active.own = true;
        own_active.origin = "mine".into();
        own_active.state = "published".into();
        own_active.manifest_ndjson =
            Some(format!("{{\"uuid\":\"{u1}\"}}\n{{\"uuid\":\"{u9}\"}}\n").into_bytes());
        upsert_package(&conn, &own_active).unwrap();

        // own but rejected -> excluded
        let mut own_rejected = sample_package("p-rej", "proj-1", "2026-07-10 00:00:00");
        own_rejected.own = true;
        own_rejected.origin = "mine".into();
        own_rejected.state = "rejected".into();
        own_rejected.manifest_ndjson = Some(format!("{{\"uuid\":\"{u1}\"}}\n").into_bytes());
        upsert_package(&conn, &own_rejected).unwrap();

        // foreign (not own) with the uuid -> excluded
        let mut foreign = sample_package("p-for", "proj-1", "2026-07-10 00:00:00");
        foreign.own = false;
        foreign.origin = "remote".into();
        foreign.state = "published".into();
        foreign.manifest_ndjson = Some(format!("{{\"uuid\":\"{u1}\"}}\n").into_bytes());
        upsert_package(&conn, &foreign).unwrap();

        let ids = own_active_announcement_ids_for_uuids(&conn, "proj-1", &[u1.to_string()]).unwrap();
        assert_eq!(ids, vec!["ann-p-own".to_string()]);

        // A uuid nobody carries yields nothing; an empty uuid list short-circuits.
        assert!(own_active_announcement_ids_for_uuids(&conn, "proj-1", &[u_none.to_string()])
            .unwrap()
            .is_empty());
        assert!(own_active_announcement_ids_for_uuids(&conn, "proj-1", &[])
            .unwrap()
            .is_empty());

        // F5: an empty (or too-short) uuid must NOT substring-match every manifest
        // — it is filtered out, so nothing is returned even though every manifest
        // trivially "contains" the empty string.
        assert!(own_active_announcement_ids_for_uuids(&conn, "proj-1", &["".to_string()])
            .unwrap()
            .is_empty());
        assert!(own_active_announcement_ids_for_uuids(&conn, "proj-1", &["u-1".to_string()])
            .unwrap()
            .is_empty(), "a short non-uuid is ignored");

        // A superseded own+active package is excluded too.
        mark_superseded(&conn, &["ann-p-own".to_string()]).unwrap();
        assert!(own_active_announcement_ids_for_uuids(&conn, "proj-1", &[u1.to_string()])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn mark_superseded_flags_only_listed() {
        let conn = test_conn();
        upsert_package(&conn, &sample_package("p1", "proj-1", "2026-07-10 00:00:00")).unwrap();
        upsert_package(&conn, &sample_package("p2", "proj-1", "2026-07-11 00:00:00")).unwrap();

        assert_eq!(mark_superseded(&conn, &[]).unwrap(), 0);
        assert_eq!(mark_superseded(&conn, &["ann-p1".to_string()]).unwrap(), 1);
        assert!(get_package(&conn, "p1").unwrap().unwrap().superseded);
        assert!(!get_package(&conn, "p2").unwrap().unwrap().superseded);
    }
}
