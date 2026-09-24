// collab_frames: catalog-side storage for the collab v3 per-frame exchange
// (wave 2). One table, owned here:
//
//   * `project_frames_local` — the local cache of every frame in a project's
//     manifest, mine or a peer's, plus the local-only fields that track where
//     it landed on disk. Per spec amendment A1 / plan ruling P26, this table
//     is the ONE source of a project frame's path: disk truth, holder
//     reports, seeding, stacking and the scanner all read it from here, never
//     from the folder layout or a header card.
//
// `origin` splits the row set in two:
//   - `'own'` — a frame I published. `source_frame_id`/`recipe_hash` are
//     mine; never pruned by `delete_not_in` (R12: a caps/manifest refresh
//     must never make my own publication disappear locally).
//   - `'replica'` — a frame a peer published, pulled down by my client.
//
// House idiom (mirrors `db/collab.rs`): a `SELECT_COLS` const paired with an
// index-based row mapper so `row.get(N)` can't drift out of sync with the
// SELECT; `anyhow::Result`, `params!`, `OptionalExtension`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use crate::collab::hub_client::FrameViewWire;

/// Column list shared by every read query, so `row_from_sql`'s index-based
/// `row.get(N)` calls can't silently drift out of sync with the SELECT.
const SELECT_COLS: &str = "project_id, frame_uuid, content_version, origin, publisher_account_id, \
    publisher_display, file_name, filter_canonical, state, accepted, byte_size, xxh3, blake3, \
    holder_count, manifest_version, manifest_json, landed_path, size_mtime_seen, on_disk, \
    locally_declined, awaiting_gc, source_frame_id, recipe_hash, last_error, updated_at";

/// Whether a `project_frames_local` row is a frame I published, or a peer's
/// that I've pulled down. Stored as `'own'` / `'replica'` (schema CHECK).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOrigin {
    Own,
    Replica,
}

impl FrameOrigin {
    fn as_db_str(self) -> &'static str {
        match self {
            FrameOrigin::Own => "own",
            FrameOrigin::Replica => "replica",
        }
    }

    fn from_db_str(s: &str) -> Self {
        match s {
            "own" => FrameOrigin::Own,
            // The schema CHECK admits only 'own'/'replica'; an unrecognized
            // value can only mean 'replica' predates a future third state —
            // never silently treat a peer's frame as mine.
            _ => FrameOrigin::Replica,
        }
    }
}

/// One cached project frame — a manifest row (mine or a peer's) plus the
/// local-only fields that track where it landed on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalFrameRow {
    pub project_id: String,
    pub frame_uuid: String,
    pub content_version: i32,
    pub origin: FrameOrigin,
    pub publisher_account_id: String,
    pub publisher_display: String,
    pub file_name: String,
    pub filter_canonical: String,
    pub state: String,
    pub accepted: bool,
    pub byte_size: i64,
    pub xxh3: String,
    pub blake3: String,
    pub holder_count: i64,
    pub manifest_version: i64,
    /// The full manifest row, verbatim (`serde_json::to_string` of the
    /// [`FrameViewWire`] this cache entry came from) — the fields above are
    /// the ones other code needs to query on; this is the fallback for
    /// everything else (`meta`, `dateObs`, `acceptedReason`, …).
    pub manifest_json: String,
    /// `None` until landed (replica) / written (own).
    pub landed_path: Option<String>,
    /// `"size:mtime_secs"` at the last verified hash.
    pub size_mtime_seen: Option<String>,
    pub on_disk: bool,
    pub locally_declined: bool,
    pub awaiting_gc: bool,
    /// Own only — the local `frames.id` this publication was generated from.
    /// Deliberately NOT a foreign key (see module docs / P19): a deleted
    /// source frame must not delete this row.
    pub source_frame_id: Option<i64>,
    /// Own only (P19) — xxh3 of the inputs that produced this publication, so
    /// a publish run can tell "nothing changed" from "needs a new version".
    pub recipe_hash: Option<String>,
    pub last_error: Option<String>,
    pub updated_at: String,
}

fn row_from_sql(row: &rusqlite::Row) -> rusqlite::Result<LocalFrameRow> {
    Ok(LocalFrameRow {
        project_id: row.get(0)?,
        frame_uuid: row.get(1)?,
        content_version: row.get(2)?,
        origin: FrameOrigin::from_db_str(&row.get::<_, String>(3)?),
        publisher_account_id: row.get(4)?,
        publisher_display: row.get(5)?,
        file_name: row.get(6)?,
        filter_canonical: row.get(7)?,
        state: row.get(8)?,
        accepted: row.get::<_, i64>(9)? != 0,
        byte_size: row.get(10)?,
        xxh3: row.get(11)?,
        blake3: row.get(12)?,
        holder_count: row.get(13)?,
        manifest_version: row.get(14)?,
        manifest_json: row.get(15)?,
        landed_path: row.get(16)?,
        size_mtime_seen: row.get(17)?,
        on_disk: row.get::<_, i64>(18)? != 0,
        locally_declined: row.get::<_, i64>(19)? != 0,
        awaiting_gc: row.get::<_, i64>(20)? != 0,
        source_frame_id: row.get(21)?,
        recipe_hash: row.get(22)?,
        last_error: row.get(23)?,
        updated_at: row.get(24)?,
    })
}

/// Apply one manifest row (mine or a peer's) to the local cache: insert it if
/// new, or refresh the manifest-derived columns if not. Never touches
/// `landed_path`/`on_disk`/`locally_declined`/`awaiting_gc`/
/// `source_frame_id`/`recipe_hash` — those are local disk/publish state that
/// a manifest fetch knows nothing about — EXCEPT: when `contentVersion`
/// increases on a `'replica'` row, this also resets `on_disk = 0` and
/// `size_mtime_seen = NULL`, because a new content version is new bytes that
/// must be fetched again (the old landed file is still the OLD version's
/// content until the next fetch lands it).
pub fn upsert_from_manifest(conn: &Connection, project_id: &str, v: &FrameViewWire) -> Result<()> {
    let manifest_json = serde_json::to_string(v)?;
    let origin = if v.own {
        FrameOrigin::Own
    } else {
        FrameOrigin::Replica
    };
    conn.execute(
        "INSERT INTO project_frames_local
            (project_id, frame_uuid, content_version, origin, publisher_account_id,
             publisher_display, file_name, filter_canonical, state, accepted, byte_size, xxh3,
             blake3, holder_count, manifest_version, manifest_json, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, datetime('now'))
         ON CONFLICT(project_id, frame_uuid) DO UPDATE SET
            content_version = excluded.content_version,
            origin = excluded.origin,
            publisher_account_id = excluded.publisher_account_id,
            publisher_display = excluded.publisher_display,
            file_name = excluded.file_name,
            filter_canonical = excluded.filter_canonical,
            state = excluded.state,
            accepted = excluded.accepted,
            byte_size = excluded.byte_size,
            xxh3 = excluded.xxh3,
            blake3 = excluded.blake3,
            holder_count = excluded.holder_count,
            manifest_version = excluded.manifest_version,
            manifest_json = excluded.manifest_json,
            on_disk = CASE
                WHEN origin = 'replica' AND excluded.content_version > content_version THEN 0
                ELSE on_disk
            END,
            size_mtime_seen = CASE
                WHEN origin = 'replica' AND excluded.content_version > content_version THEN NULL
                ELSE size_mtime_seen
            END,
            updated_at = datetime('now')",
        params![
            project_id,
            v.frame_uuid,
            v.content_version,
            origin.as_db_str(),
            v.publisher_account_id,
            v.publisher_display_name,
            v.file_name,
            v.filter_canonical,
            v.state,
            v.accepted,
            v.byte_size,
            v.xxh3,
            v.blake3,
            v.holder_count,
            v.manifest_version,
            manifest_json,
        ],
    )?;
    Ok(())
}

/// One cached frame, if present.
pub fn get(conn: &Connection, project_id: &str, frame_uuid: &str) -> Result<Option<LocalFrameRow>> {
    conn.query_row(
        &format!("SELECT {SELECT_COLS} FROM project_frames_local WHERE project_id = ?1 AND frame_uuid = ?2"),
        params![project_id, frame_uuid],
        row_from_sql,
    )
    .optional()
    .map_err(Into::into)
}

/// Every cached frame of a project, ordered by frame_uuid.
pub fn list_for_project(conn: &Connection, project_id: &str) -> Result<Vec<LocalFrameRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLS} FROM project_frames_local WHERE project_id = ?1 ORDER BY frame_uuid"
    ))?;
    let rows = stmt
        .query_map(params![project_id], row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The caps-rule prune (P9): delete every `'replica'` row of a project whose
/// `frame_uuid` is NOT in `keep` — an empty set deletes every replica row.
/// NEVER deletes `origin = 'own'` rows (R12). Returns the number removed.
pub fn delete_not_in(conn: &Connection, project_id: &str, keep: &HashSet<String>) -> Result<usize> {
    let sql = if keep.is_empty() {
        "DELETE FROM project_frames_local WHERE project_id = ?1 AND origin = 'replica'".to_string()
    } else {
        let placeholders = vec!["?"; keep.len()].join(", ");
        format!(
            "DELETE FROM project_frames_local \
             WHERE project_id = ? AND origin = 'replica' AND frame_uuid NOT IN ({placeholders})"
        )
    };
    let params_iter = std::iter::once(project_id.to_string()).chain(keep.iter().cloned());
    let removed = conn.execute(&sql, rusqlite::params_from_iter(params_iter))?;
    Ok(removed)
}

/// Record (or fully replace) an own-frame row — the publish path's write,
/// distinct from [`upsert_from_manifest`] because it carries the local-only
/// fields (`landed_path`, `source_frame_id`, `recipe_hash`, …) that a
/// manifest fetch never has.
pub fn record_own(conn: &Connection, row: &LocalFrameRow) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO project_frames_local
            (project_id, frame_uuid, content_version, origin, publisher_account_id,
             publisher_display, file_name, filter_canonical, state, accepted, byte_size, xxh3,
             blake3, holder_count, manifest_version, manifest_json, landed_path, size_mtime_seen,
             on_disk, locally_declined, awaiting_gc, source_frame_id, recipe_hash, last_error,
             updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                 ?19, ?20, ?21, ?22, ?23, ?24, datetime('now'))",
        params![
            row.project_id,
            row.frame_uuid,
            row.content_version,
            row.origin.as_db_str(),
            row.publisher_account_id,
            row.publisher_display,
            row.file_name,
            row.filter_canonical,
            row.state,
            row.accepted,
            row.byte_size,
            row.xxh3,
            row.blake3,
            row.holder_count,
            row.manifest_version,
            row.manifest_json,
            row.landed_path,
            row.size_mtime_seen,
            row.on_disk,
            row.locally_declined,
            row.awaiting_gc,
            row.source_frame_id,
            row.recipe_hash,
            row.last_error,
        ],
    )?;
    Ok(())
}

/// Mark a frame landed on disk: sets `landed_path`/`size_mtime_seen`,
/// `on_disk = 1`, `awaiting_gc = 0`, and clears `last_error`.
pub fn set_landed(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    landed_path: &str,
    size_mtime: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE project_frames_local
         SET landed_path = ?3, size_mtime_seen = ?4, on_disk = 1, awaiting_gc = 0,
             last_error = NULL, updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid, landed_path, size_mtime],
    )?;
    Ok(())
}

/// Mark a frame's content missing from disk (`on_disk = 0`); `awaiting_gc`
/// records whether it's missing because the store's own GC dropped it (P20 —
/// never `Copy`-repaired, re-fetched once the entry is `NotFound`) or for
/// some other reason (e.g. disk truth found the landed file gone).
pub fn set_missing(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    awaiting_gc: bool,
) -> Result<()> {
    conn.execute(
        "UPDATE project_frames_local SET on_disk = 0, awaiting_gc = ?3, updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid, awaiting_gc],
    )?;
    Ok(())
}

/// Record a new `size:mtime` for a landed file whose content re-hashed to
/// the recorded `xxh3` (disk truth, spec §5.5: touched but identical).
/// Column-targeted; returns the rows touched.
pub fn set_size_mtime_seen(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    size_mtime: &str,
) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE project_frames_local SET size_mtime_seen = ?3, updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid, size_mtime],
    )?)
}

/// Set (or clear) `locally_declined` on a batch of frames of one project — the
/// loss guard's "stop holding" step (P14) and any future manual decline.
pub fn set_declined(
    conn: &Connection,
    project_id: &str,
    frame_uuids: &[String],
    declined: bool,
) -> Result<()> {
    if frame_uuids.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; frame_uuids.len()].join(", ");
    let sql = format!(
        "UPDATE project_frames_local SET locally_declined = ?, updated_at = datetime('now') \
         WHERE project_id = ? AND frame_uuid IN ({placeholders})"
    );
    let mut vals: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(frame_uuids.len() + 2);
    vals.push(&declined);
    vals.push(&project_id);
    for u in frame_uuids {
        vals.push(u);
    }
    conn.execute(&sql, vals.as_slice())?;
    Ok(())
}

/// Record (or clear, `None`) a frame's last error — landing/fetch failure,
/// probe failure, etc.
pub fn set_error(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    error: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE project_frames_local SET last_error = ?3, updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid, error],
    )?;
    Ok(())
}

/// The cached frame currently landed at a given path, if any (`landed_path`
/// is globally UNIQUE across every project). Used by the scanner's
/// reconciliation pass (P26).
pub fn find_by_landed_path(conn: &Connection, path: &str) -> Result<Option<LocalFrameRow>> {
    conn.query_row(
        &format!("SELECT {SELECT_COLS} FROM project_frames_local WHERE landed_path = ?1"),
        params![path],
        row_from_sql,
    )
    .optional()
    .map_err(Into::into)
}

/// Every cached frame of a project with the given content hash — the
/// "moved or duplicate" leg of the scanner's reconciliation (P26) and of
/// [`P24`](crate)-style byte-identical sharing.
pub fn find_by_project_and_xxh3(
    conn: &Connection,
    project_id: &str,
    xxh3: &str,
) -> Result<Vec<LocalFrameRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLS} FROM project_frames_local WHERE project_id = ?1 AND xxh3 = ?2"
    ))?;
    let rows = stmt
        .query_map(params![project_id, xxh3], row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Correct a frame's recorded path without touching its disk-state flags —
/// the scanner's "moved" repair once it has matched a relocated file back to
/// its row by `(project, xxh3)`.
pub fn update_landed_path(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    path: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE project_frames_local SET landed_path = ?3, updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid, path],
    )?;
    Ok(())
}

/// Every own-frame row of a project, keyed by its `source_frame_id` — the
/// publish run's "what have I already published for this local frame" index
/// (P19's recipe-hash comparison).
pub fn own_by_source_frame(
    conn: &Connection,
    project_id: &str,
) -> Result<HashMap<i64, LocalFrameRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLS} FROM project_frames_local \
         WHERE project_id = ?1 AND origin = 'own' AND source_frame_id IS NOT NULL"
    ))?;
    let rows = stmt
        .query_map(params![project_id], row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|r| r.source_frame_id.map(|id| (id, r)))
        .collect())
}

/// The parent directory of any already-landed row for a publisher in a
/// project (P10): the folder every later frame from that publisher reuses.
/// `None` when nothing from that publisher has landed yet.
pub fn publisher_dir(
    conn: &Connection,
    project_id: &str,
    publisher_account_id: &str,
) -> Result<Option<PathBuf>> {
    let landed: Option<String> = conn
        .query_row(
            "SELECT landed_path FROM project_frames_local
             WHERE project_id = ?1 AND publisher_account_id = ?2 AND landed_path IS NOT NULL
             LIMIT 1",
            params![project_id, publisher_account_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(landed.and_then(|p| PathBuf::from(p).parent().map(PathBuf::from)))
}

/// Store a new recipe hash on an own frame whose regeneration came out
/// byte-identical (P19). Column-targeted: every hub-owned column a concurrent
/// manifest sync may have written (`state`, `accepted`, `manifest_version`,
/// `holder_count`, …) is left alone. Returns the rows touched.
pub fn set_recipe_hash(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    recipe_hash: &str,
) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE project_frames_local SET recipe_hash = ?3, updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid, recipe_hash],
    )?)
}

/// Record a new content version of an own frame (P19): the version, its
/// hashes and size, the recipe that produced it, the landed file's
/// `size:mtime`, and the same content keys inside `manifest_json`.
/// Column-targeted like [`set_recipe_hash`] — hub-owned columns survive.
#[allow(clippy::too_many_arguments)]
pub fn set_own_version(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    content_version: i32,
    blake3: &str,
    xxh3: &str,
    byte_size: i64,
    recipe_hash: &str,
    size_mtime_seen: Option<&str>,
) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE project_frames_local
         SET content_version = ?3, blake3 = ?4, xxh3 = ?5, byte_size = ?6, recipe_hash = ?7,
             size_mtime_seen = ?8, on_disk = 1, awaiting_gc = 0, last_error = NULL,
             manifest_json = CASE WHEN json_valid(manifest_json)
                 THEN json_set(manifest_json, '$.contentVersion', ?3, '$.blake3', ?4,
                               '$.xxh3', ?5, '$.byteSize', ?6)
                 ELSE manifest_json END,
             updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2",
        params![
            project_id,
            frame_uuid,
            content_version,
            blake3,
            xxh3,
            byte_size,
            recipe_hash,
            size_mtime_seen
        ],
    )?)
}

/// Bind an own frame the hub already knows (a manifest-delivered row, or one
/// the hub refused as "already announced") to the local frame it was
/// generated from: `source_frame_id`, `landed_path`, `recipe_hash`,
/// `size_mtime_seen`, `on_disk = 1`. Column-targeted — hub-owned columns
/// survive. Returns the rows touched.
pub fn adopt_own(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    source_frame_id: i64,
    landed_path: &str,
    recipe_hash: &str,
    size_mtime_seen: Option<&str>,
) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE project_frames_local
         SET source_frame_id = ?3, landed_path = ?4, recipe_hash = ?5, size_mtime_seen = ?6,
             on_disk = 1, awaiting_gc = 0, last_error = NULL, updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2 AND origin = 'own'",
        params![
            project_id,
            frame_uuid,
            source_frame_id,
            landed_path,
            recipe_hash,
            size_mtime_seen
        ],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&c).unwrap();
        c.execute(
            "INSERT INTO collab_projects
                (project_id, slug, title, data_role, target_name, target_ra_deg, target_dec_deg,
                 target_radius_deg, membership_version, snapshot_payload_b64,
                 snapshot_signature_b64, members_json)
             VALUES ('p1','m31','M31','send_receive','M31',10.7,41.3,1.5,1,'x','x','[]')",
            [],
        )
        .unwrap();
        c
    }

    fn view(uuid: &str, mv: i64) -> FrameViewWire {
        serde_json::from_value(serde_json::json!({
            "frameUuid": uuid, "publisherAccountId":"a1","publisherDisplayName":"Ann","own":false,
            "fileName": format!("c_{uuid}.fits"),"contentVersion":1,"blake3":"b".repeat(64),"byteSize":100,
            "xxh3":"0123456789abcdef","filterRaw":"Red","filterCanonical":"R","channel":"mono","exptimeSec":300.0,
            "meta":{},"gateVersion":0,"accepted":true,"state":"published","manifestVersion":mv,
            "createdAt":"2026-09-24T00:00:00Z","holderCount":1}))
        .unwrap()
    }

    #[test]
    fn manifest_upsert_keeps_local_disk_state() {
        let c = conn();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        set_landed(
            &c,
            "p1",
            "u1",
            "/collab/m31/ann/c_u1.fits",
            "100:1700000000",
        )
        .unwrap();
        let mut v = view("u1", 2);
        v.accepted = false;
        v.accepted_reason = Some("clouds".into());
        upsert_from_manifest(&c, "p1", &v).unwrap();
        let r = get(&c, "p1", "u1").unwrap().unwrap();
        assert!(r.on_disk && !r.accepted);
        assert_eq!(r.landed_path.as_deref(), Some("/collab/m31/ann/c_u1.fits"));
        assert_eq!(r.manifest_version, 2);
    }

    #[test]
    fn prune_never_deletes_own_rows() {
        let c = conn();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        upsert_from_manifest(&c, "p1", &view("u2", 1)).unwrap();
        let mut own = get(&c, "p1", "u2").unwrap().unwrap();
        own.origin = FrameOrigin::Own;
        record_own(&c, &own).unwrap();
        let n = delete_not_in(&c, "p1", &HashSet::new()).unwrap();
        assert_eq!(n, 1);
        assert!(get(&c, "p1", "u2").unwrap().is_some());
    }

    #[test]
    fn publisher_dir_is_the_parent_of_an_existing_landing() {
        let c = conn();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        set_landed(&c, "p1", "u1", "/collab/m31/ann/c_u1.fits", "100:1").unwrap();
        assert_eq!(
            publisher_dir(&c, "p1", "a1").unwrap(),
            Some(PathBuf::from("/collab/m31/ann"))
        );
        assert_eq!(publisher_dir(&c, "p1", "zz").unwrap(), None);
    }

    #[test]
    fn project_delete_cascades() {
        let c = conn();
        c.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        c.execute("DELETE FROM collab_projects WHERE project_id='p1'", [])
            .unwrap();
        assert!(list_for_project(&c, "p1").unwrap().is_empty());
    }

    /// The version-bump branch of [`upsert_from_manifest`]'s `CASE` expression:
    /// a `'replica'` row whose `contentVersion` increases must be reset to
    /// "not on disk" — the landed file is still the OLD content until the next
    /// fetch lands the new version. Same-version and version-decrease upserts
    /// (should never happen, but the CASE is `>`, not `!=`) must NOT reset it.
    #[test]
    fn version_bump_resets_replica_disk_state_but_not_own() {
        let c = conn();
        // Replica: version bump clears on_disk / size_mtime_seen.
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        set_landed(&c, "p1", "u1", "/collab/m31/ann/c_u1.fits", "100:1").unwrap();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap(); // same version: no reset
        let same = get(&c, "p1", "u1").unwrap().unwrap();
        assert!(same.on_disk && same.size_mtime_seen.is_some());
        let mut bumped = view("u1", 1);
        bumped.content_version = 2;
        upsert_from_manifest(&c, "p1", &bumped).unwrap();
        let after = get(&c, "p1", "u1").unwrap().unwrap();
        assert!(!after.on_disk);
        assert!(after.size_mtime_seen.is_none());

        // Own: a version bump must NOT clear on_disk — my own newly-published
        // version's bytes are already on disk, written by the publish run
        // itself, not fetched from a peer.
        let mut own_v1 = view("u2", 1);
        own_v1.own = true;
        upsert_from_manifest(&c, "p1", &own_v1).unwrap();
        set_landed(&c, "p1", "u2", "/collab/m31/me/c_u2.fits", "100:1").unwrap();
        let mut own_v2 = view("u2", 1);
        own_v2.own = true;
        own_v2.content_version = 2;
        upsert_from_manifest(&c, "p1", &own_v2).unwrap();
        let own_after = get(&c, "p1", "u2").unwrap().unwrap();
        assert!(
            own_after.on_disk,
            "an own row's disk state is not manifest-driven"
        );
    }

    /// R10: the publish write-backs touch only their own columns — a
    /// `state`/`accepted`/`manifest_version` written by a concurrent manifest
    /// sync survives all three.
    #[test]
    fn own_write_backs_leave_hub_columns_alone() {
        let c = conn();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        let mut own = get(&c, "p1", "u1").unwrap().unwrap();
        own.origin = FrameOrigin::Own;
        own.source_frame_id = None;
        record_own(&c, &own).unwrap();
        c.execute(
            "UPDATE project_frames_local SET state = 'rejected', accepted = 0, manifest_version = 42",
            [],
        )
        .unwrap();

        assert_eq!(
            adopt_own(&c, "p1", "u1", 7, "/c/m31/me/c_u1.fits", "r0", Some("1:2")).unwrap(),
            1
        );
        assert_eq!(set_recipe_hash(&c, "p1", "u1", "r1").unwrap(), 1);
        assert_eq!(
            set_own_version(
                &c,
                "p1",
                "u1",
                2,
                &"c".repeat(64),
                "fedcba9876543210",
                555,
                "r2",
                Some("555:9")
            )
            .unwrap(),
            1
        );
        let r = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(
            (r.state.as_str(), r.accepted, r.manifest_version),
            ("rejected", false, 42)
        );
        assert_eq!(r.source_frame_id, Some(7));
        assert_eq!(r.landed_path.as_deref(), Some("/c/m31/me/c_u1.fits"));
        assert_eq!(r.content_version, 2);
        assert_eq!(r.recipe_hash.as_deref(), Some("r2"));
        assert_eq!(r.byte_size, 555);
        assert!(r.on_disk);
        let m: serde_json::Value = serde_json::from_str(&r.manifest_json).unwrap();
        assert_eq!(m["contentVersion"], 2);
        assert_eq!(m["byteSize"], 555);
        assert_eq!(m["xxh3"], "fedcba9876543210");
        assert_eq!(
            m["state"], "published",
            "manifest_json keeps its other keys"
        );

        // adopt_own never touches a replica row.
        upsert_from_manifest(&c, "p1", &view("u2", 1)).unwrap();
        assert_eq!(adopt_own(&c, "p1", "u2", 8, "/x", "r", None).unwrap(), 0);
    }
}
