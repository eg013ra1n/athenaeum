// collab_frames: catalog-side storage for the collab v3 per-frame exchange
// (wave 2). Two tables, owned here:
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
//   * `collab_foreign_files` — files under the Collaboration root the scanner
//     matched to no row above (P26 "unknown"): listed inert for R18's "not
//     part of the project" list, never catalogued.
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
    locally_declined, awaiting_gc, source_frame_id, recipe_hash, last_error, updated_at, \
    local_state, frame_seq";

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

/// The wave-3 per-frame local state machine (plan P8): where a cached frame
/// stands relative to my disk, independent of the hub's own `state`
/// (moderation) column. `on_disk` is kept in lockstep with [`servable`] —
/// [`set_local_state`] is the ONE writer that moves both together (plus the
/// claim/outbox change a servability flip causes, C24); the wave-2 writers
/// (`set_landed`, `set_missing`, `set_declined`, `record_own`) still move
/// `on_disk` directly, in step with `local_state`, until Tasks 9/11/15
/// replace them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocalState {
    /// A replica frame I don't have and haven't declined — the need set.
    Wanted,
    /// On disk, servable to the hub/peers.
    Held,
    /// Was on disk, isn't any more (GC'd, deleted, disk truth found it gone).
    Missing,
    /// A version bump landed while the old file was quarantined/edited; the
    /// user must resolve which content to keep.
    AwaitingChoice,
    /// Disk truth found the landed file changed in place (size/hash mismatch
    /// against what I fetched) — the Changed files list (L5).
    Quarantined,
    /// Locally declined ("stop holding") — never fetched again until undone.
    NotKept,
    /// Not part of my replication scope right now (caps-excluded, not yet
    /// published/accepted) — nothing to do.
    Idle,
    /// My own frame, on disk.
    OwnHeld,
    /// My own frame, not on disk (the source file is gone/moved).
    OwnMissing,
    /// My own frame, staged with new bytes the hub hasn't confirmed yet.
    OwnChanged,
}

impl LocalState {
    pub fn as_db_str(self) -> &'static str {
        match self {
            LocalState::Wanted => "wanted",
            LocalState::Held => "held",
            LocalState::Missing => "missing",
            LocalState::AwaitingChoice => "awaiting_choice",
            LocalState::Quarantined => "quarantined",
            LocalState::NotKept => "not_kept",
            LocalState::Idle => "idle",
            LocalState::OwnHeld => "own_held",
            LocalState::OwnMissing => "own_missing",
            LocalState::OwnChanged => "own_changed",
        }
    }

    /// An unrecognized value (a corrupt row, or a state name from a future
    /// version this build doesn't know) never panics and never silently
    /// picks a servable state — it defaults to `Wanted` and logs so the drift
    /// is visible instead of swallowed.
    pub fn from_db_str(s: &str) -> LocalState {
        match s {
            "wanted" => LocalState::Wanted,
            "held" => LocalState::Held,
            "missing" => LocalState::Missing,
            "awaiting_choice" => LocalState::AwaitingChoice,
            "quarantined" => LocalState::Quarantined,
            "not_kept" => LocalState::NotKept,
            "idle" => LocalState::Idle,
            "own_held" => LocalState::OwnHeld,
            "own_missing" => LocalState::OwnMissing,
            "own_changed" => LocalState::OwnChanged,
            other => {
                tracing::warn!(value = %other, "unknown local_state value, defaulting to wanted");
                LocalState::Wanted
            }
        }
    }

    /// Whether this state means the frame's bytes are servable to the hub and
    /// to peers right now. Kept equal to `on_disk` by every writer.
    pub fn servable(self) -> bool {
        matches!(self, LocalState::Held | LocalState::OwnHeld)
    }
}

/// The result of a [`set_local_state`] transition: the state moved from/to,
/// and the claim-set change it caused (`None` when servability didn't flip).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateWrite {
    pub from: LocalState,
    pub to: LocalState,
    pub claim: Option<crate::db::collab_live::ClaimOp>,
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
    /// The wave-3 local state machine (P8). Written only by the functions
    /// documented on [`LocalState`]; a manifest fetch ([`upsert_from_manifest`])
    /// only sets it for a brand-new row (see that function's docs).
    pub local_state: LocalState,
    /// This device's position in the project's holder-map sequence — `None`
    /// until the hub assigns one (announce/confirm). Set only by
    /// [`set_frame_seq`].
    pub frame_seq: Option<i32>,
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
        local_state: LocalState::from_db_str(
            &row.get::<_, Option<String>>(25)?.unwrap_or_default(),
        ),
        frame_seq: row.get(26)?,
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
             blake3, manifest_version, manifest_json, frame_seq, local_state, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                 CASE WHEN ?16 > 0 THEN ?16 ELSE NULL END,
                 CASE WHEN ?4 = 'own' THEN 'own_missing'
                      WHEN ?9 = 'published' AND ?10 = 1 THEN 'wanted'
                      ELSE 'idle' END,
                 datetime('now'))
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
            manifest_version = excluded.manifest_version,
            manifest_json = excluded.manifest_json,
            frame_seq = CASE WHEN ?16 > 0 THEN ?16 ELSE frame_seq END,
            on_disk = CASE
                WHEN origin = 'replica' AND excluded.content_version > content_version THEN 0
                ELSE on_disk
            END,
            -- Interim rule (Task 9 replaces this with the full edge set): a
            -- version bump only ever downgrades a currently-`held` replica
            -- back to `wanted` (new bytes to fetch); every other state
            -- (`missing`, `awaiting_choice`, `quarantined`, `not_kept`,
            -- `idle`, `wanted` itself) is untouched by a manifest refresh.
            local_state = CASE
                WHEN origin = 'replica' AND excluded.content_version > content_version
                     AND local_state = 'held' THEN 'wanted'
                ELSE local_state
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
            v.manifest_version,
            manifest_json,
            v.frame_seq,
        ],
    )?;
    Ok(())
}

/// Move a frame to `to`, keeping `on_disk` equal to "servable" and appending
/// the claim change to the outbox when servability flips (plan P8, C24).
/// Returns the transition, or `None` when the row does not exist. The caller
/// supplies the transaction; nothing is committed here — a caller that wants
/// the state change and its claim/outbox row atomic passes a
/// [`rusqlite::Transaction`] and commits (or rolls back) itself.
pub fn set_local_state(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    to: LocalState,
) -> Result<Option<StateWrite>> {
    let Some((from_raw, cv)): Option<(Option<String>, i32)> = conn
        .query_row(
            "SELECT local_state, content_version FROM project_frames_local WHERE project_id = ?1 AND frame_uuid = ?2",
            params![project_id, frame_uuid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let from = LocalState::from_db_str(from_raw.as_deref().unwrap_or("wanted"));
    conn.execute(
        "UPDATE project_frames_local SET local_state = ?3, on_disk = ?4,
            state_changed_at = datetime('now'), updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid, to.as_db_str(), to.servable()],
    )?;
    let claim = match (from.servable(), to.servable()) {
        (false, true) => Some(crate::db::collab_live::ClaimOp::Add {
            content_version: cv,
        }),
        (true, false) => Some(crate::db::collab_live::ClaimOp::Remove),
        _ => None,
    };
    if let Some(op) = claim {
        crate::db::collab_live::record_claim_change(conn, project_id, frame_uuid, op)?;
    }
    Ok(Some(StateWrite { from, to, claim }))
}

/// Set this device's holder-map sequence for a frame (P8: the number the hub
/// assigns on announce/confirm). Column-targeted; returns the rows touched.
pub fn set_frame_seq(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    frame_seq: i32,
) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE project_frames_local SET frame_seq = ?3, updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid, frame_seq],
    )?)
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
    let condition = if keep.is_empty() {
        String::new()
    } else {
        let placeholders = vec!["?"; keep.len()].join(", ");
        format!(" AND frame_uuid NOT IN ({placeholders})")
    };
    // A servable ('held') replica row about to be deleted must drop its
    // local claim too, through the outbox (T5/T9 ruling): otherwise the
    // device keeps reporting a claim on a frame it no longer has any local
    // row for at all, and the hub's copy is never told to stop.
    let doomed: Vec<String> = {
        let sql = format!(
            "SELECT frame_uuid FROM project_frames_local \
             WHERE project_id = ?1 AND origin = 'replica' AND local_state IN ('held', 'own_held'){condition}"
        );
        let params_iter = std::iter::once(project_id.to_string()).chain(keep.iter().cloned());
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params_iter), |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for frame_uuid in &doomed {
        crate::db::collab_live::record_claim_change(
            conn,
            project_id,
            frame_uuid,
            crate::db::collab_live::ClaimOp::Remove,
        )?;
    }
    let sql = format!(
        "DELETE FROM project_frames_local WHERE project_id = ?1 AND origin = 'replica'{condition}"
    );
    let params_iter = std::iter::once(project_id.to_string()).chain(keep.iter().cloned());
    let removed = conn.execute(&sql, rusqlite::params_from_iter(params_iter))?;
    Ok(removed)
}

/// Record (or fully replace) an own-frame row — the publish path's write,
/// distinct from [`upsert_from_manifest`] because it carries the local-only
/// fields (`landed_path`, `source_frame_id`, `recipe_hash`, …) that a
/// manifest fetch never has.
pub fn record_own(conn: &Connection, row: &LocalFrameRow) -> Result<()> {
    // `INSERT OR REPLACE` resets every column left out of the list to its
    // schema default, so `frame_seq` and `local_state` — real columns since
    // wave 3 — MUST be carried through explicitly or a re-publish of an
    // already-published frame would silently wipe them back to NULL.
    // `local_state` is derived fresh from `on_disk` (and `origin`, for the
    // rare test fixture that seeds a 'replica' row through this path), not
    // copied from `row.local_state`: this is the interim rule until Task 9's
    // full edge set (an own row is exactly `own_held`/`own_missing` by its
    // `on_disk`, mirroring `set_landed`/`set_missing`'s own/replica split).
    let local_state = match (row.origin, row.on_disk) {
        (FrameOrigin::Own, true) => LocalState::OwnHeld,
        (FrameOrigin::Own, false) => LocalState::OwnMissing,
        (FrameOrigin::Replica, true) => LocalState::Held,
        (FrameOrigin::Replica, false) => LocalState::Missing,
    };
    conn.execute(
        "INSERT OR REPLACE INTO project_frames_local
            (project_id, frame_uuid, content_version, origin, publisher_account_id,
             publisher_display, file_name, filter_canonical, state, accepted, byte_size, xxh3,
             blake3, holder_count, manifest_version, manifest_json, landed_path, size_mtime_seen,
             on_disk, locally_declined, awaiting_gc, source_frame_id, recipe_hash, last_error,
             local_state, frame_seq, state_changed_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                 ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, datetime('now'), datetime('now'))",
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
            local_state.as_db_str(),
            row.frame_seq,
        ],
    )?;
    Ok(())
}

/// Mark a frame landed on disk: sets `landed_path`/`size_mtime_seen`,
/// `on_disk = 1`, `awaiting_gc = 0`, and clears `last_error` and
/// `rejected_size_mtime`.
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
             last_error = NULL, rejected_size_mtime = NULL,
             local_state = CASE WHEN origin = 'own' THEN 'own_held' ELSE 'held' END,
             updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid, landed_path, size_mtime],
    )?;
    Ok(())
}

/// [`set_landed`] only while the row still describes the bytes that landed —
/// the same `content_version` AND `blake3` (a manifest sync can move either
/// while a fetch runs, ruling R20). Returns the rows touched: 0 means the
/// landing is stale and nothing was written.
#[allow(clippy::too_many_arguments)]
pub fn set_landed_if(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    landed_path: &str,
    size_mtime: &str,
    content_version: i32,
    blake3: &str,
) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE project_frames_local
         SET landed_path = ?3, size_mtime_seen = ?4, on_disk = 1, awaiting_gc = 0,
             last_error = NULL, rejected_size_mtime = NULL,
             local_state = CASE WHEN origin = 'own' THEN 'own_held' ELSE 'held' END,
             updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2 AND content_version = ?5 AND blake3 = ?6",
        params![
            project_id,
            frame_uuid,
            landed_path,
            size_mtime,
            content_version,
            blake3
        ],
    )?)
}

/// The `size:mtime` of the file at the row's landed path that re-admission
/// last rejected (R21), if any.
pub fn rejected_size_mtime(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT rejected_size_mtime FROM project_frames_local
             WHERE project_id = ?1 AND frame_uuid = ?2",
            params![project_id, frame_uuid],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// Remember that re-admission rejected the file at `size_mtime` (R21).
pub fn set_rejected_size_mtime(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    size_mtime: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE project_frames_local SET rejected_size_mtime = ?3
         WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid, size_mtime],
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
        "UPDATE project_frames_local SET on_disk = 0, awaiting_gc = ?3,
             local_state = CASE WHEN origin = 'own' THEN 'own_missing' ELSE 'missing' END,
             updated_at = datetime('now')
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
    // Interim rule (Task 15 replaces this with the full state machine):
    // declining moves straight to `not_kept`; undoing a decline moves back to
    // `wanted` — this is the loss guard's own "stop holding"/"resume" toggle,
    // never called on an own row.
    let local_state = if declined {
        LocalState::NotKept
    } else {
        LocalState::Wanted
    };
    let local_state_str = local_state.as_db_str();
    let placeholders = vec!["?"; frame_uuids.len()].join(", ");
    let sql = format!(
        "UPDATE project_frames_local SET locally_declined = ?, local_state = ?, updated_at = datetime('now') \
         WHERE project_id = ? AND frame_uuid IN ({placeholders})"
    );
    let mut vals: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(frame_uuids.len() + 3);
    vals.push(&declined);
    vals.push(&local_state_str);
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

/// The local half of a new own content version, written the moment the
/// regenerated file replaced the landed one and was seeded — BEFORE the hub
/// confirms the version (final-review I2). Only the columns that describe the
/// file on disk move (`xxh3`, `byte_size`, `size_mtime_seen`, `on_disk = 1`,
/// `awaiting_gc = 0`), so disk truth sees the new file as present instead of
/// "edited". The hub-confirmed columns (`content_version`, `blake3`) stay
/// until [`set_own_version`], and `recipe_hash` is cleared: an empty recipe
/// matches no current recipe, so ANY later publish run — not only a
/// republish — regenerates the frame, finds bytes the hub does not have, and
/// posts the version a dead run never confirmed (a republish or a plate solve
/// moves the bytes without moving the recipe). [`set_own_version`] writes the
/// recipe back. Only a row still landed at `landed_path` is touched. Returns
/// the rows touched.
pub fn stage_own_file(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    landed_path: &str,
    xxh3: &str,
    byte_size: i64,
    size_mtime_seen: Option<&str>,
) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE project_frames_local
         SET xxh3 = ?4, byte_size = ?5, size_mtime_seen = ?6, on_disk = 1, awaiting_gc = 0,
             recipe_hash = NULL, updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2 AND origin = 'own' AND landed_path = ?3",
        params![
            project_id,
            frame_uuid,
            landed_path,
            xxh3,
            byte_size,
            size_mtime_seen
        ],
    )?)
}

/// Undo [`stage_own_file`] after the hub refused the new version: the file
/// at the landed path now holds bytes the hub never took, so the row goes
/// back to the hub's content keys and is marked NOT on disk (the old version
/// is gone). Disk truth then rejects the file (its size / xxh3 no longer
/// match), and the next publish run regenerates and posts again (the recipe
/// [`stage_own_file`] cleared stays cleared, so a plain run does). Returns the
/// rows touched.
pub fn unstage_own_file(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    hub_xxh3: &str,
    hub_byte_size: i64,
) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE project_frames_local
         SET xxh3 = ?3, byte_size = ?4, size_mtime_seen = NULL, on_disk = 0, awaiting_gc = 0,
             updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2 AND origin = 'own'",
        params![project_id, frame_uuid, hub_xxh3, hub_byte_size],
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

/// Every project id cached on this device — the scope the scanner's
/// `(project, xxh3)` lookup walks when a file carries no `ATH_PRJ` stamp.
pub fn project_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT project_id FROM collab_projects ORDER BY project_id")?;
    let ids = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ids)
}

/// List `path` as a foreign file under the Collaboration root (P26 "unknown",
/// R18), with the project its `ATH_PRJ` stamp names, if any, and the
/// `size:mtime` it was hashed at (R30). Idempotent: a re-scan refreshes
/// `seen_at`, the stamp and `size_mtime`, one row per path.
pub fn record_foreign_file(
    conn: &Connection,
    path: &str,
    project_id: Option<&str>,
    size_mtime: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO collab_foreign_files (path, project_id, size_mtime, seen_at)
         VALUES (?1, ?2, ?3, datetime('now'))
         ON CONFLICT(path) DO UPDATE SET project_id = excluded.project_id,
                                         size_mtime = excluded.size_mtime,
                                         seen_at = excluded.seen_at",
        params![path, project_id, size_mtime],
    )?;
    Ok(())
}

/// The `size:mtime` a listed foreign file was hashed at: `None` when `path`
/// is not listed, `Some(None)` for a row without one.
pub fn foreign_file_size_mtime(conn: &Connection, path: &str) -> Result<Option<Option<String>>> {
    conn.query_row(
        "SELECT size_mtime FROM collab_foreign_files WHERE path = ?1",
        params![path],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Drop `path` from the foreign list — the scanner matched it to a frame row
/// after all (known or moved). Returns the rows removed.
pub fn forget_foreign_file(conn: &Connection, path: &str) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM collab_foreign_files WHERE path = ?1",
        params![path],
    )?)
}

/// After a COMPLETED walk of the Collaboration root `root`: drop every listed
/// foreign file under it that the walk did not see (deleted or moved away).
/// Returns the rows removed.
pub fn prune_foreign_files_under(
    conn: &Connection,
    root: &str,
    seen: &HashSet<String>,
) -> Result<usize> {
    let (pred, values) =
        crate::db::scan_root_prefix_predicate("path", std::slice::from_ref(&root.to_string()));
    let listed: Vec<String> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT path FROM collab_foreign_files WHERE {pred}"
        ))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values.iter()), |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        rows
    };
    let mut removed = 0;
    for path in listed.iter().filter(|p| !seen.contains(*p)) {
        removed += forget_foreign_file(conn, path)?;
    }
    Ok(removed)
}

/// Drop every listed foreign file under `root` — the Collaboration root was
/// cleared, so its list no longer means anything. Returns the rows removed.
pub fn delete_foreign_files_under(conn: &Connection, root: &str) -> Result<usize> {
    let (pred, values) =
        crate::db::scan_root_prefix_predicate("path", std::slice::from_ref(&root.to_string()));
    Ok(conn.execute(
        &format!("DELETE FROM collab_foreign_files WHERE {pred}"),
        rusqlite::params_from_iter(values.iter()),
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

    /// T5/T9 ruling: a deleted replica row that was `held` (servable) must
    /// not leak its claim — the removal goes through the outbox like any
    /// other claim change, so the next report tells the hub to drop it.
    #[test]
    fn prune_removes_a_kept_rows_claim_through_the_outbox() {
        let c = conn();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        upsert_from_manifest(&c, "p1", &view("u2", 1)).unwrap();
        set_landed(&c, "p1", "u1", "/collab/m31/ann/u1.fits", "100:1").unwrap();
        set_landed(&c, "p1", "u2", "/collab/m31/ann/u2.fits", "100:1").unwrap();
        let keep: HashSet<String> = ["u2".to_string()].into_iter().collect();
        let n = delete_not_in(&c, "p1", &keep).unwrap();
        assert_eq!(n, 1);
        assert!(get(&c, "p1", "u1").unwrap().is_none());
        assert!(get(&c, "p1", "u2").unwrap().is_some(), "kept row untouched");
        use crate::db::collab_live::{outbox, ClaimOp};
        let rows = outbox(&c, "p1").unwrap();
        assert_eq!(rows.len(), 1, "only the deleted row's claim is dropped");
        assert_eq!(rows[0].frame_uuid, "u1");
        assert_eq!(rows[0].op, ClaimOp::Remove);
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

    /// R20: `set_landed_if` lands only on the version and content it was
    /// asked for; a moved row stays untouched.
    #[test]
    fn set_landed_if_refuses_a_moved_version() {
        let c = conn();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        let r = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(
            set_landed_if(
                &c,
                "p1",
                "u1",
                "/x/a.fits",
                "1:1",
                r.content_version + 1,
                &r.blake3
            )
            .unwrap(),
            0
        );
        assert_eq!(
            set_landed_if(
                &c,
                "p1",
                "u1",
                "/x/a.fits",
                "1:1",
                r.content_version,
                "f00d"
            )
            .unwrap(),
            0
        );
        assert!(!get(&c, "p1", "u1").unwrap().unwrap().on_disk);
        set_rejected_size_mtime(&c, "p1", "u1", "9:9").unwrap();
        assert_eq!(
            rejected_size_mtime(&c, "p1", "u1").unwrap().as_deref(),
            Some("9:9")
        );
        assert_eq!(
            set_landed_if(
                &c,
                "p1",
                "u1",
                "/x/a.fits",
                "1:1",
                r.content_version,
                &r.blake3
            )
            .unwrap(),
            1
        );
        let landed = get(&c, "p1", "u1").unwrap().unwrap();
        assert!(landed.on_disk);
        assert_eq!(landed.landed_path.as_deref(), Some("/x/a.fits"));
        assert_eq!(
            rejected_size_mtime(&c, "p1", "u1").unwrap(),
            None,
            "a landing clears it"
        );
    }

    #[test]
    fn set_local_state_writes_on_disk_and_the_claim_in_one_transaction() {
        let conn = conn();
        upsert_from_manifest(&conn, "p1", &view("u1", 1)).unwrap();
        assert_eq!(
            get(&conn, "p1", "u1").unwrap().unwrap().local_state,
            LocalState::Wanted
        );

        let tx = conn.unchecked_transaction().unwrap();
        let w = set_local_state(&tx, "p1", "u1", LocalState::Held)
            .unwrap()
            .unwrap();
        assert_eq!((w.from, w.to), (LocalState::Wanted, LocalState::Held));
        assert_eq!(
            w.claim,
            Some(crate::db::collab_live::ClaimOp::Add { content_version: 1 })
        );
        tx.rollback().unwrap();
        // rolled back together: no state change, no outbox row
        assert_eq!(
            get(&conn, "p1", "u1").unwrap().unwrap().local_state,
            LocalState::Wanted
        );
        assert_eq!(crate::db::collab_live::outbox_len(&conn, "p1").unwrap(), 0);

        set_local_state(&conn, "p1", "u1", LocalState::Held).unwrap();
        let row = get(&conn, "p1", "u1").unwrap().unwrap();
        assert!(row.on_disk);
        // non-servable → non-servable writes no claim
        set_local_state(&conn, "p1", "u1", LocalState::Missing).unwrap();
        let w = set_local_state(&conn, "p1", "u1", LocalState::AwaitingChoice)
            .unwrap()
            .unwrap();
        assert_eq!(w.claim, None);
        assert!(!get(&conn, "p1", "u1").unwrap().unwrap().on_disk);
        assert_eq!(crate::db::collab_live::outbox_len(&conn, "p1").unwrap(), 2);
        // add, remove
    }

    #[test]
    fn unpublished_or_excluded_replicas_start_idle() {
        let conn = conn();
        let mut v = view("u2", 1);
        v.accepted = false;
        upsert_from_manifest(&conn, "p1", &v).unwrap();
        assert_eq!(
            get(&conn, "p1", "u2").unwrap().unwrap().local_state,
            LocalState::Idle
        );
        let mut p = view("u3", 1);
        p.state = "pending".into();
        upsert_from_manifest(&conn, "p1", &p).unwrap();
        assert_eq!(
            get(&conn, "p1", "u3").unwrap().unwrap().local_state,
            LocalState::Idle
        );
    }

    /// Fix round 1, Important #1: a manifest-first own row (R8a — this
    /// account's own frame, published from another device, arrives here
    /// before the local publish/adopt path ever runs, e.g. a rebuilt local
    /// DB) must NOT claim a servable state it can't back. It starts
    /// `own_missing`/`on_disk = 0`; only [`record_own`] (once the bytes
    /// really land) moves it to `own_held`/`on_disk = 1`.
    #[test]
    fn manifest_first_own_row_starts_own_missing_then_record_own_lands_it() {
        let conn = conn();
        let mut v = view("u4", 1);
        v.own = true;
        upsert_from_manifest(&conn, "p1", &v).unwrap();
        let row = get(&conn, "p1", "u4").unwrap().unwrap();
        assert_eq!(row.local_state, LocalState::OwnMissing);
        assert!(!row.on_disk);

        let mut landed = row;
        landed.landed_path = Some("/collab/m31/me/c_u4.fits".into());
        landed.on_disk = true;
        record_own(&conn, &landed).unwrap();
        let row = get(&conn, "p1", "u4").unwrap().unwrap();
        assert_eq!(row.local_state, LocalState::OwnHeld);
        assert!(row.on_disk);
    }
}
