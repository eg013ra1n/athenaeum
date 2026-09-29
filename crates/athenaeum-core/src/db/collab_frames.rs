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
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use crate::collab::hub_client::FrameViewWire;

/// Column list shared by every read query, so `row_from_sql`'s index-based
/// `row.get(N)` calls can't silently drift out of sync with the SELECT.
///
/// `holder_count` and `locally_declined` stay in the table, never read (wave
/// 3 Task 15: the live holder map answers the count, `not_kept` replaced the
/// decline).
const SELECT_COLS: &str = "project_id, frame_uuid, content_version, origin, publisher_account_id, \
    publisher_display, file_name, filter_canonical, state, accepted, byte_size, xxh3, blake3, \
    manifest_version, manifest_json, landed_path, size_mtime_seen, on_disk, \
    awaiting_gc, source_frame_id, recipe_hash, last_error, updated_at, \
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
/// claim/outbox change a servability flip causes, C24); `set_landed` and
/// `record_own` still move `on_disk` directly, in step with `local_state`.
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
    /// sets it for a brand-new row and otherwise moves it only through the
    /// state machine's manifest edges (see that function's docs).
    pub local_state: LocalState,
    /// The hub's dense per-project frame ordinal (`frameSeq`: assigned at
    /// announce, never reused) — the key holder claims, `holders` deltas and
    /// snapshot runs name a frame by. NOT the holder cursor (`holder_seq`).
    /// `None` until the manifest or a holder snapshot delivers it; written by
    /// [`upsert_from_manifest`] and [`set_frame_seq`], and carried through by
    /// [`record_own`].
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
        manifest_version: row.get(13)?,
        manifest_json: row.get(14)?,
        landed_path: row.get(15)?,
        size_mtime_seen: row.get(16)?,
        on_disk: row.get::<_, i64>(17)? != 0,
        awaiting_gc: row.get::<_, i64>(18)? != 0,
        source_frame_id: row.get(19)?,
        recipe_hash: row.get(20)?,
        last_error: row.get(21)?,
        updated_at: row.get(22)?,
        local_state: LocalState::from_db_str(
            &row.get::<_, Option<String>>(23)?.unwrap_or_default(),
        ),
        frame_seq: row.get(24)?,
    })
}

/// Apply one manifest row (mine or a peer's) to the local cache: insert it if
/// new, or refresh the manifest-derived columns if not. The UPSERT never
/// writes `local_state`/`on_disk`/`landed_path`/
/// `awaiting_gc`/`source_frame_id`/`recipe_hash` — local disk/publish state
/// a manifest fetch knows nothing about. A brand-new row starts `own_missing`
/// (own), `wanted` (a published, accepted replica) or `idle`.
///
/// For an EXISTING row the previous row is read first (on the caller's
/// connection / transaction) and the manifest's move is fed to the state
/// machine (`collab::storage::states`, spec §9.4, Task 9), each resulting
/// move through [`set_local_state`] (claim + outbox in the same transaction,
/// C24):
///
/// 1. a new `contentVersion` → `NewVersion { same_bytes: blake3 unchanged }`
///    — a `held` replica with new bytes goes back to `wanted` (its old file
///    stays until the new version replaces it, L7) and its
///    `size_mtime_seen` is cleared; with the same bytes it stays `held` and
///    is re-claimed at the new version (P10); `quarantined`/`not_kept`/
///    `awaiting_choice`/`idle` keep their state (L5, L6);
/// 2. then a publish-state move — published ∧ accepted lost → `Excluded`
///    (`wanted`/`held`/`awaiting_choice`/`missing` → `idle`: file kept, not
///    served, not fetched; [`exclude_to_idle`]); regained → `Reincluded`
///    (`idle` → `held` only for a known-seeded row whose file stats the
///    same, else `wanted` with its landed file handed to the storage engine
///    — [`reinclude`]). Nothing is hashed here: this runs in the caller's
///    write transaction.
///
/// For a caller OUTSIDE a transaction (autocommit): the landed file of a
/// re-included frame is handed to the storage engine right away. A caller
/// inside a transaction uses [`upsert_from_manifest_deferred`] and routes the
/// collected [`EngineRoutes`] only after its commit, so the engine never
/// reads a row the transaction might still roll back.
pub fn upsert_from_manifest(conn: &Connection, project_id: &str, v: &FrameViewWire) -> Result<()> {
    let mut routes = EngineRoutes::default();
    upsert_from_manifest_deferred(conn, project_id, v, &mut routes)?;
    routes.route();
    Ok(())
}

/// The landed files of re-included frames that [`upsert_from_manifest_deferred`]
/// hands to the storage engine once the caller has committed.
#[derive(Debug, Default)]
#[must_use = "route the collected files after the transaction commits"]
pub struct EngineRoutes(Vec<(String, String, std::path::PathBuf)>);

impl EngineRoutes {
    /// Hand every collected file to the running storage engine (see
    /// [`route_to_engine`]). Call after the transaction that collected them
    /// committed.
    pub fn route(self) {
        for (project_id, frame_uuid, path) in self.0 {
            route_to_engine(&project_id, &frame_uuid, &path);
        }
    }
}

/// [`upsert_from_manifest`] for a caller inside a write transaction: the
/// files to hand to the storage engine are collected into `routes`, to be
/// routed with [`EngineRoutes::route`] after the commit.
pub fn upsert_from_manifest_deferred(
    conn: &Connection,
    project_id: &str,
    v: &FrameViewWire,
    routes: &mut EngineRoutes,
) -> Result<()> {
    use crate::collab::storage::states::{transition, StateEvent};

    let manifest_json = serde_json::to_string(v)?;
    let origin = if v.own {
        FrameOrigin::Own
    } else {
        FrameOrigin::Replica
    };
    let prev = get(conn, project_id, &v.frame_uuid)?;
    conn.execute(
        "INSERT INTO project_frames_local
            (project_id, frame_uuid, content_version, origin, publisher_account_id,
             publisher_display, file_name, filter_canonical, state, accepted, byte_size, xxh3,
             blake3, manifest_version, manifest_json, frame_seq, local_state, state_changed_at,
             updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                 CASE WHEN ?16 > 0 THEN ?16 ELSE NULL END,
                 CASE WHEN ?4 = 'own' THEN 'own_missing'
                      WHEN ?9 = 'published' AND ?10 = 1 THEN 'wanted'
                      ELSE 'idle' END,
                 datetime('now'), datetime('now'))
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
    let Some(prev) = prev else {
        return Ok(());
    };
    if prev.origin != origin {
        return change_origin(conn, project_id, v, &prev, origin, routes);
    }

    // An own row staged with new bytes (`stage_own_file`) whose version the
    // hub now confirms with exactly those bytes: same xxh3 and size as the
    // staged file, a new blake3 — no longer staged (Task 10, C11).
    if prev.origin == FrameOrigin::Own
        && prev.xxh3 == v.xxh3
        && prev.byte_size == v.byte_size
        && prev.blake3 != v.blake3
    {
        conn.execute(
            "UPDATE project_frames_local SET own_staged = 0
             WHERE project_id = ?1 AND frame_uuid = ?2 AND own_staged = 1",
            params![project_id, v.frame_uuid],
        )?;
    }

    let mut state = prev.local_state;
    // 1. a new content version
    if prev.content_version != v.content_version {
        let same_bytes = prev.blake3 == v.blake3;
        let ev = StateEvent::NewVersion { same_bytes };
        if let Some(to) = transition(prev.origin, state, ev) {
            if to != state {
                set_local_state(conn, project_id, &v.frame_uuid, to)?;
                tracing::debug!(
                    project_id,
                    frame_uuid = %v.frame_uuid,
                    content_version = v.content_version,
                    from_state = state.as_db_str(),
                    to_state = to.as_db_str(),
                    "new content version moved the local state"
                );
                state = to;
            } else if same_bytes && to.servable() {
                // P10: the same bytes under a new version — re-claim at it.
                crate::db::collab_live::record_claim_change(
                    conn,
                    project_id,
                    &v.frame_uuid,
                    crate::db::collab_live::ClaimOp::Add {
                        content_version: v.content_version,
                    },
                )?;
            }
        }
        if !same_bytes && prev.origin == FrameOrigin::Replica {
            // New bytes: the recorded stamp describes the OLD content, and a
            // dead store entry of the old hash no longer stands in the way
            // of a fetch (fix round 1: a parked frame must not stay parked
            // forever). A quarantined row keeps its stamp — its file is the
            // user's to resolve.
            //
            // L5/C38 (Task 11 fix rounds 1+2): every row whose stamp this
            // clears keeps it as `prev_stamp` — a `wanted` row AND an `idle`
            // one (an excluded frame the engine ignores: an edit made then
            // must still be caught on re-inclusion). A stamp verified at the
            // current version is always the better description of the file
            // on disk; on a second bump while still wanted `size_mtime_seen`
            // is already NULL, so the FIRST stamp survives. The landing and
            // the engine's pre-bump check tell the untouched old version
            // from an edit nothing watched by it.
            conn.execute(
                "UPDATE project_frames_local
                 SET prev_stamp = COALESCE(size_mtime_seen, prev_stamp),
                     size_mtime_seen = NULL, awaiting_gc = 0
                 WHERE project_id = ?1 AND frame_uuid = ?2 AND local_state <> 'quarantined'",
                params![project_id, v.frame_uuid],
            )?;
        }
    }
    // 2. a publish-state move (after the version edge: both can arrive in
    //    one manifest row)
    if prev.origin == FrameOrigin::Replica {
        let published = v.state == "published" && v.accepted;
        let was_published = prev.state == "published" && prev.accepted;
        let ev = match (was_published, published) {
            (true, false) => Some(StateEvent::Excluded),
            (false, true) => Some(StateEvent::Reincluded),
            _ => None,
        };
        if let Some(ev) = ev {
            if transition(prev.origin, state, ev).is_some() {
                let Some(row) = get(conn, project_id, &v.frame_uuid)? else {
                    return Ok(());
                };
                let to = if ev == StateEvent::Reincluded {
                    let (to, route) = reinclude(conn, &row)?;
                    if let Some(path) = route {
                        routes
                            .0
                            .push((project_id.to_string(), v.frame_uuid.clone(), path));
                    }
                    to
                } else {
                    exclude_to_idle(conn, &row)?
                };
                if to != state {
                    tracing::debug!(
                        project_id,
                        frame_uuid = %v.frame_uuid,
                        from_state = state.as_db_str(),
                        to_state = to.as_db_str(),
                        "publish state moved the local state"
                    );
                }
            }
        }
    }
    // 3. a claim the hub refused (P9) is reported again once the hub lists
    //    the frame anew — the only input that can change its verdict. A hub
    //    restore that lost the frame refuses every claim that reaches it
    //    before the publisher re-announces it under the same uuid; the row
    //    never left `held`, so without this the holding stayed off the hub
    //    for good (both digests agree on the missing claim).
    if prev.last_error.as_deref() == Some(crate::db::collab_live::REFUSED_CLAIM_ERROR)
        && ensure_claimed(conn, project_id, &v.frame_uuid)?.is_some()
    {
        set_error(conn, project_id, &v.frame_uuid, None)?;
        tracing::info!(
            project_id,
            frame_uuid = %v.frame_uuid,
            content_version = v.content_version,
            "a refused claim is reported again: the hub lists the frame"
        );
    }
    Ok(())
}

/// An existing row whose origin the manifest changed (amendment A6: `own`
/// is derived per DEVICE from `publisherDeviceId`, no longer per account).
/// The UPSERT above already wrote the new origin; this moves the local
/// state to what a row of that origin starts as, never leaving an own
/// state on a replica (or the reverse):
///
/// - own → replica (a frame another device of this account published —
///   the pre-A6 derivation had cached it as own): the own-only columns go
///   (`source_frame_id`, `recipe_hash`, `own_staged`); the state is what a
///   new replica gets (`wanted` when published and accepted, else `idle`).
///   A file at the recorded path is handed to the storage engine after the
///   commit, which re-adopts it by hash (no transfer) or quarantines it.
/// - replica → own (this device's key is the frame's publisher): `held`
///   becomes `own_held`, `quarantined` becomes `own_changed` (its file
///   changed: not served, the owner's to republish or put back — final fix
///   B-M1), anything else `own_missing`. The quarantine record of a
///   quarantined replica goes in the same write: an own frame's change is
///   its owner's, never a "Changed files" entry, so no record is orphaned.
/// - Accepted edge (final review B-M3): an own frame's file may lie OUTSIDE
///   the Collaboration root (own frames live wherever their owner keeps
///   them). Turned replica, that path is routed to the storage engine, but
///   no engine watches it — the row stays `wanted` and is fetched into the
///   root like any replica, a second copy beside the file outside.
fn change_origin(
    conn: &Connection,
    project_id: &str,
    v: &FrameViewWire,
    prev: &LocalFrameRow,
    origin: FrameOrigin,
    routes: &mut EngineRoutes,
) -> Result<()> {
    let to = match origin {
        FrameOrigin::Replica => {
            conn.execute(
                "UPDATE project_frames_local
                 SET source_frame_id = NULL, recipe_hash = NULL, own_staged = 0
                 WHERE project_id = ?1 AND frame_uuid = ?2",
                params![project_id, v.frame_uuid],
            )?;
            if v.state == "published" && v.accepted {
                LocalState::Wanted
            } else {
                LocalState::Idle
            }
        }
        FrameOrigin::Own => match prev.local_state {
            LocalState::Held => LocalState::OwnHeld,
            LocalState::Quarantined => LocalState::OwnChanged,
            _ => LocalState::OwnMissing,
        },
    };
    set_local_state(conn, project_id, &v.frame_uuid, to)?;
    if prev.local_state == LocalState::Quarantined && origin == FrameOrigin::Own {
        crate::db::collab_live::unquarantine(conn, project_id, &v.frame_uuid)?;
    }
    if to == LocalState::Wanted {
        if let Some(path) = prev.landed_path.as_deref().map(std::path::PathBuf::from) {
            if path.is_file() {
                routes
                    .0
                    .push((project_id.to_string(), v.frame_uuid.clone(), path));
            }
        }
    }
    tracing::info!(
        project_id,
        frame_uuid = %v.frame_uuid,
        from_state = prev.local_state.as_db_str(),
        to_state = to.as_db_str(),
        outcome = if origin == FrameOrigin::Own { "own" } else { "replica" },
        "frame origin changed with its publishing device"
    );
    Ok(())
}

/// Hand a re-included frame's landed file to the running storage engine,
/// which re-adopts it (hash off the async runtime, seed, verify) or
/// quarantines it when its bytes changed. No engine → the frame simply
/// stays wanted.
fn route_to_engine(project_id: &str, frame_uuid: &str, path: &std::path::Path) {
    if !crate::collab::storage::watch::route_touched(path) {
        tracing::debug!(
            project_id,
            frame_uuid,
            path = %path.display(),
            "no storage engine watches this file; the re-included frame stays wanted"
        );
    }
}

/// Move a replica to `idle` (excluded / lost project / policy drop). A
/// frame excluded from a SERVABLE state keeps its seed tags and its stamp —
/// the stamp is this row's "known seeded" record, which [`reinclude`]
/// trusts. One excluded from any other state was not seeded, so its stamp
/// is cleared (fix round 1). Returns the new state.
pub fn exclude_to_idle(conn: &Connection, row: &LocalFrameRow) -> Result<LocalState> {
    use crate::collab::storage::states::{transition, StateEvent};
    let Some(to) = transition(row.origin, row.local_state, StateEvent::Excluded) else {
        return Ok(row.local_state);
    };
    if to != row.local_state {
        set_local_state(conn, &row.project_id, &row.frame_uuid, to)?;
    }
    if !row.local_state.servable() {
        conn.execute(
            "UPDATE project_frames_local SET size_mtime_seen = NULL
             WHERE project_id = ?1 AND frame_uuid = ?2",
            params![row.project_id, row.frame_uuid],
        )?;
    }
    Ok(to)
}

/// Re-include an `idle` replica (spec §9.4 "Idle ──re-included──▶ Held or
/// Wanted (stat + hash decide)"), without hashing: this runs inside the
/// caller's write transaction (fix round 1).
///
/// - `held` only for a row KNOWN to be seeded — a recorded stamp (kept only
///   by an exclusion from a servable state), not parked (`awaiting_gc = 0`)
///   — whose file still stats the same.
/// - otherwise `wanted`, the stamp kept, and the landed file (when present)
///   returned for the caller to hand to the storage engine AFTER its write,
///   which re-adopts it by hash or quarantines it when the bytes changed.
///
/// `awaiting_gc` is cleared either way. Returns the new state and the path
/// to route.
pub fn reinclude(
    conn: &Connection,
    row: &LocalFrameRow,
) -> Result<(LocalState, Option<std::path::PathBuf>)> {
    use crate::collab::storage::states::{transition, StateEvent};
    use crate::collab::storage::sweep::{stat_verdict, Stamp, StatVerdict};
    if transition(row.origin, row.local_state, StateEvent::Reincluded).is_none() {
        return Ok((row.local_state, None));
    }
    let path = row.landed_path.as_deref().map(std::path::PathBuf::from);
    let known_seeded = row.size_mtime_seen.is_some() && !row.awaiting_gc;
    let same = match (&path, known_seeded) {
        (Some(p), true) => matches!(
            stat_verdict(p, row.size_mtime_seen.as_deref().and_then(Stamp::parse)),
            StatVerdict::Same
        ),
        _ => false,
    };
    let to = if same {
        LocalState::Held
    } else {
        LocalState::Wanted
    };
    conn.execute(
        "UPDATE project_frames_local SET awaiting_gc = 0 WHERE project_id = ?1 AND frame_uuid = ?2",
        params![row.project_id, row.frame_uuid],
    )?;
    set_local_state(conn, &row.project_id, &row.frame_uuid, to)?;
    let route = match (&path, same) {
        (Some(p), false) if p.is_file() => Some(p.clone()),
        _ => None,
    };
    Ok((to, route))
}

/// Whether any cached frame has exactly `byte_size` bytes — the cheap
/// pre-filter before an unknown file is fully hashed (fix round 1).
pub fn any_with_byte_size(conn: &Connection, byte_size: i64) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM project_frames_local WHERE byte_size = ?1 LIMIT 1",
            params![byte_size],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Frames parked over a dead collab-store entry (`awaiting_gc` on a
/// `wanted`/`own_missing` row with a recorded path) — the storage engine's
/// retry set (fix round 1).
pub fn parked_rows(conn: &Connection) -> Result<Vec<LocalFrameRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLS} FROM project_frames_local \
         WHERE awaiting_gc = 1 AND landed_path IS NOT NULL \
           AND local_state IN ('wanted', 'own_missing') ORDER BY project_id, frame_uuid"
    ))?;
    let rows = stmt
        .query_map([], row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Every cached frame (any project) with content hash `blake3` — the
/// siblings a dead store entry takes down with it (C10, fix round 1).
pub fn rows_with_blake3(conn: &Connection, blake3: &str) -> Result<Vec<LocalFrameRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLS} FROM project_frames_local WHERE blake3 = ?1 ORDER BY project_id, frame_uuid"
    ))?;
    let rows = stmt
        .query_map(params![blake3], row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
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
    // `prev_stamp` (Task 11 fix round 2, L5/C38) is cleared only by a newer
    // truth: ENTERING `held` / `own_held` (a stamp verified at the current
    // version supersedes it) or `quarantined` (the quarantine record does).
    // Every other move keeps it — `wanted → idle → wanted` included, so an
    // edit made while excluded is still caught after re-inclusion.
    conn.execute(
        "UPDATE project_frames_local SET local_state = ?3, on_disk = ?4,
            prev_stamp = CASE WHEN ?3 IN ('held', 'own_held', 'quarantined')
                              THEN NULL ELSE prev_stamp END,
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

/// The `size:mtime` a `wanted` or `idle` row's landed file was last
/// verified at, kept when a new version (or a parked frame's release)
/// cleared `size_mtime_seen` (Task 11 fix rounds 1+2, L5/C38). Cleared only
/// on entering `held`, `own_held` or `quarantined`, by a recorded landing,
/// or by [`set_landed`]. `None` when not recorded or the row does not exist.
pub fn prev_stamp(conn: &Connection, project_id: &str, frame_uuid: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT prev_stamp FROM project_frames_local WHERE project_id = ?1 AND frame_uuid = ?2",
            params![project_id, frame_uuid],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// Set a frame's hub frame ordinal (`frameSeq`, the dense per-project number
/// the hub assigns at announce — what holder claims are keyed by; not the
/// holder cursor). Column-targeted; returns the rows touched.
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
    // The claim-rm reads/inserts and the delete itself run as one
    // transaction (fix round M1): a crash or a concurrent reader must never
    // observe the rows gone without their claims already dropped, or the
    // claims dropped while the rows still exist to be re-scanned as "doomed"
    // a second time.
    // IMMEDIATE: it reads before it writes; a deferred read-to-write upgrade
    // under another writer fails at once with SQLITE_BUSY, never waiting the
    // busy timeout (the collab live feed's full reload runs this).
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
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
        let mut stmt = tx.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params_iter), |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for frame_uuid in &doomed {
        crate::db::collab_live::record_claim_change(
            &tx,
            project_id,
            frame_uuid,
            crate::db::collab_live::ClaimOp::Remove,
        )?;
    }
    let sql = format!(
        "DELETE FROM project_frames_local WHERE project_id = ?1 AND origin = 'replica'{condition}"
    );
    let params_iter = std::iter::once(project_id.to_string()).chain(keep.iter().cloned());
    let removed = tx.execute(&sql, rusqlite::params_from_iter(params_iter))?;
    tx.commit()?;
    Ok(removed)
}

/// Record a NEW own-frame row — the publish path's write for a frame it has
/// just announced (or adopted with no row yet), distinct from
/// [`upsert_from_manifest`] because it carries the local-only fields
/// (`landed_path`, `source_frame_id`, `recipe_hash`, …) that a manifest fetch
/// never has.
///
/// New rows only: an EXISTING own row is changed through the column-targeted
/// writers ([`adopt_own`], [`stage_own_file`], [`set_own_version`],
/// [`unstage_own_file`]), which move `local_state` through
/// [`set_local_state`] with its claim/outbox change. This write sets
/// `local_state` directly and records no claim — the caller owns the claim (an
/// announce's implicit `(uuid, 1)`, or an adoption's outbox `add`). If a
/// manifest sync inserted the same row between the announce and this write,
/// its hub-assigned `frame_seq` survives (`COALESCE`), so a row argument
/// without one never wipes it.
pub fn record_own(conn: &Connection, row: &LocalFrameRow) -> Result<()> {
    // `INSERT OR REPLACE` resets every column left out of the list to its
    // schema default, so `frame_seq` and `local_state` — real columns since
    // wave 3 — MUST be carried through explicitly or a re-publish of an
    // already-published frame would silently wipe them back to NULL.
    // `local_state` is derived fresh from `on_disk` (and `origin`, for the
    // rare test fixture that seeds a 'replica' row through this path), not
    // copied from `row.local_state`: this is the interim rule until Task 9's
    // full edge set (an own row is exactly `own_held`/`own_missing` by its
    // `on_disk`, mirroring `set_landed`'s own/replica split).
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
             blake3, manifest_version, manifest_json, landed_path, size_mtime_seen,
             on_disk, awaiting_gc, source_frame_id, recipe_hash, last_error,
             local_state, frame_seq, state_changed_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                 ?19, ?20, ?21, ?22, ?23,
                 COALESCE(?24, (SELECT frame_seq FROM project_frames_local
                                WHERE project_id = ?1 AND frame_uuid = ?2)),
                 datetime('now'), datetime('now'))",
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
            row.manifest_version,
            row.manifest_json,
            row.landed_path,
            row.size_mtime_seen,
            row.on_disk,
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
/// `on_disk = 1`, `awaiting_gc = 0`, and clears `last_error`,
/// `rejected_size_mtime` and `prev_stamp` (a re-admitted version must not
/// carry an older version's stamp into the next bump — Task 11 fix round 2).
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
             last_error = NULL, rejected_size_mtime = NULL, prev_stamp = NULL,
             local_state = CASE WHEN origin = 'own' THEN 'own_held' ELSE 'held' END,
             state_changed_at = CASE
                 WHEN local_state IS (CASE WHEN origin = 'own' THEN 'own_held' ELSE 'held' END)
                 THEN state_changed_at ELSE datetime('now') END,
             updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid, landed_path, size_mtime],
    )?;
    Ok(())
}

/// Record a landing's path and stamp only while the row still describes the
/// bytes that landed — the same `content_version` AND `blake3` (a manifest
/// sync can move either while a fetch runs, ruling R20; the landing fence,
/// I1) — AND is still `wanted` (P13: nothing lands over a quarantined, not
/// kept or awaiting-choice frame, spec §7.5). Clears `awaiting_gc`,
/// `last_error` and `rejected_size_mtime`. It no longer moves `on_disk` or
/// `local_state`: the caller runs [`set_local_state`] (`held`, with its
/// outbox `add`) in the same transaction (collab v3 wave 3, Task 11).
/// Returns the rows touched: 0 means the landing is stale and nothing was
/// written.
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
         SET landed_path = ?3, size_mtime_seen = ?4, awaiting_gc = 0,
             last_error = NULL, rejected_size_mtime = NULL, prev_stamp = NULL,
             updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2 AND content_version = ?5 AND blake3 = ?6
           AND local_state = 'wanted'",
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

/// Park (`on`) or release a frame waiting for the collab store's GC to drop
/// a dead entry (P20): nothing else moves — a `wanted` row stays `wanted`,
/// and the need set skips it while `awaiting_gc = 1` (Task 15: the landing
/// and the executor's dead-entry check park; the executor's GC probe
/// releases). Returns the rows touched.
pub fn set_awaiting_gc(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    on: bool,
) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE project_frames_local SET awaiting_gc = ?3, updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid, on],
    )?)
}

/// When each `wanted` row of a project became wanted (`state_changed_at`,
/// UTC), as epoch milliseconds — the scheduler's starvation clock (spec
/// §7.2). A value that does not parse is left out (the caller reads "now").
pub fn wanted_since(conn: &Connection, project_id: &str) -> Result<HashMap<String, i64>> {
    let mut stmt = conn.prepare(
        "SELECT frame_uuid, state_changed_at FROM project_frames_local
         WHERE project_id = ?1 AND local_state = 'wanted'",
    )?;
    let rows = stmt.query_map(params![project_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (uuid, at) = row?;
        if let Some(ms) = at.as_deref().and_then(|s| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                .ok()
                .map(|t| t.and_utc().timestamp_millis())
        }) {
            out.insert(uuid, ms);
        }
    }
    Ok(out)
}

/// Rows parked by a landing for the collab store's GC (`awaiting_gc = 1`,
/// no recorded stamp — a row the storage engine PARKED at a moved file
/// keeps its stamp and is retried by the engine instead): `(project,
/// frame, blake3)`.
pub fn awaiting_gc_released(conn: &Connection) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT project_id, frame_uuid, blake3 FROM project_frames_local
         WHERE awaiting_gc = 1 AND size_mtime_seen IS NULL
         ORDER BY project_id, frame_uuid",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
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

/// A fetch's failure on its row, only while the row is still `wanted` —
/// the live executor's error write runs off its loop (final fix A-I2), and a
/// write that waited out another writer must never mark a row that landed
/// or moved on meanwhile. Returns the rows written (0 or 1).
pub fn set_fetch_error(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    error: &str,
) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE project_frames_local SET last_error = ?3, updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2 AND COALESCE(local_state, 'wanted') = 'wanted'",
        params![project_id, frame_uuid, error],
    )?)
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

/// Every cached frame whose `landed_path` is `dir` itself or lies under it
/// (separator-strict: `/c/M31` never matches `/c/M31_Ha/x.fits`) — a removed
/// folder settles as ONE path, and every landing beneath it is gone with it
/// (Task 9, from the Task 8 review). Ordered by `(project_id, frame_uuid)`.
pub fn rows_under(conn: &Connection, dir: &str) -> Result<Vec<LocalFrameRow>> {
    let (pred, mut values) = crate::db::scan_root_prefix_predicate(
        "landed_path",
        std::slice::from_ref(&dir.to_string()),
    );
    values.insert(0, rusqlite::types::Value::Text(dir.to_string()));
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLS} FROM project_frames_local \
         WHERE landed_path = ? OR ({pred}) ORDER BY project_id, frame_uuid"
    ))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(values.iter()), row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Every cached frame with a recorded `landed_path`, own rows outside the
/// Collaboration root included (spec §9.2, amendment A1) — the stat sweep's
/// input. Ordered by `(project_id, frame_uuid)`.
pub fn rows_with_landed_path(conn: &Connection) -> Result<Vec<LocalFrameRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLS} FROM project_frames_local \
         WHERE landed_path IS NOT NULL ORDER BY project_id, frame_uuid"
    ))?;
    let rows = stmt
        .query_map([], row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Every cached frame of a project in one of `states` — the "Changed
/// files", "Not kept" and deletion-choice lists. An empty `states` matches
/// nothing. Ordered by `frame_uuid`.
pub fn list_by_state(
    conn: &Connection,
    project_id: &str,
    states: &[LocalState],
) -> Result<Vec<LocalFrameRow>> {
    if states.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; states.len()].join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLS} FROM project_frames_local \
         WHERE project_id = ? AND local_state IN ({placeholders}) ORDER BY frame_uuid"
    ))?;
    let values = std::iter::once(project_id.to_string())
        .chain(states.iter().map(|s| s.as_db_str().to_string()));
    let rows = stmt
        .query_map(rusqlite::params_from_iter(values), row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
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
/// its row by `(project, xxh3)`. Returns the rows touched (0 or 1: the
/// caller decides whether that means "no such row" or a real failure — for
/// instance `landed_path` is `TEXT UNIQUE` table-wide, so a write that would
/// collide with another project's row raises a rusqlite error rather than
/// returning here at all; a `Ok(0)` return is a plain no-match).
pub fn update_landed_path(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    path: &str,
) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE project_frames_local SET landed_path = ?3, updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid, path],
    )?)
}

/// The scanner's repair of a moved frame's path with no storage engine
/// running (final fix B-M4): the path, and the stamp CLEARED — the collab
/// store's entry still names the old path, so the row is not "verified at
/// this path" until the engine's next check re-seeds it there (or parks it
/// over the dead entry); it never stays held on a dead entry.
pub fn repair_moved_landed_path(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    path: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE project_frames_local SET landed_path = ?3, size_mtime_seen = NULL,
            updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid, path],
    )?;
    Ok(())
}

/// Every `fileName` a publisher (account) has in a project's cached
/// manifest — what a new own frame's name must not reuse (amendment A6: a
/// second device of the account may have published under the same name).
pub fn file_names_of_publisher(
    conn: &Connection,
    project_id: &str,
    publisher_account_id: &str,
) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT file_name FROM project_frames_local
         WHERE project_id = ?1 AND publisher_account_id = ?2",
    )?;
    let names = stmt
        .query_map(params![project_id, publisher_account_id], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    Ok(names)
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
/// project (P10) that lies under `root`, the current Collaboration folder:
/// the folder every later frame from that publisher reuses. A landing under
/// a previous folder (a re-designation, Task 15 fix round 1) or an own
/// frame's file outside it never counts. `None` when nothing from that
/// publisher has landed under `root` yet.
///
/// C1 fix round 3: a candidate `landed_path` is trusted as "this publisher's
/// own folder" only when it is BOTH:
///   1. not an attested original — `recipe_hash` starting `external:` marks
///      one (the same test [`crate::api::collab::was_external_recipe`] uses
///      independently at the removal site), and
///   2. shaped like a generated landing — [`crate::api::collab_exchange::publisher_folder`]
///      always writes `<root>/<project-slug>/<own-folder-name>/<file>`, so
///      the file's grandparent is always exactly `<root>/<project-slug>/`,
///      never the Collaboration root itself or anything shallower/deeper.
/// Either filter alone already excludes an attested original that happens to
/// sit at `<root>/<some-folder>/<file>` (spec §10 allows attestation of a
/// set anywhere under the root, including the user's own working folder);
/// both are kept because neither is airtight alone (a mid-flight
/// `stage_own_file` clears `recipe_hash` to `NULL` before the row is
/// re-classified, and a path spelling drift could in principle satisfy the
/// shape check). Trusting an attested original's folder as "own" would let a
/// later generation step write into — and its cleanup delete out of — a
/// directory this publisher never actually owns (C1).
pub fn publisher_dir(
    conn: &Connection,
    project_id: &str,
    publisher_account_id: &str,
    root: &Path,
) -> Result<Option<PathBuf>> {
    let slug: Option<String> = conn
        .query_row(
            "SELECT slug FROM collab_projects WHERE project_id = ?1",
            params![project_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(slug) = slug else {
        return Ok(None);
    };
    let project_dir = root.join(crate::sync::ingest::sanitize_slug(&slug));
    let mut stmt = conn.prepare(
        "SELECT landed_path, recipe_hash FROM project_frames_local
         WHERE project_id = ?1 AND publisher_account_id = ?2 AND landed_path IS NOT NULL
         ORDER BY frame_uuid",
    )?;
    let rows = stmt
        .query_map(params![project_id, publisher_account_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter(|(_, recipe)| !recipe.as_deref().is_some_and(|r| r.starts_with("external:")))
        .map(|(p, _)| PathBuf::from(p))
        .find(|p| p.parent().and_then(Path::parent) == Some(project_dir.as_path()))
        .and_then(|p| p.parent().map(PathBuf::from)))
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
/// Column-targeted like [`set_recipe_hash`] — hub-owned columns survive. The
/// row moves to `own_held` through [`set_local_state`] (T1/T6 ruling), in one
/// savepoint with the column write: a row that was not servable gets its
/// outbox `add`. The hub's implicit claim on the new version is the caller's
/// ([`crate::db::collab_live::add_implicit_claim`]).
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
    let sp = crate::db::operations::SavepointGuard::new(conn, "set_own_version")?;
    let n = conn.execute(
        "UPDATE project_frames_local
         SET content_version = ?3, blake3 = ?4, xxh3 = ?5, byte_size = ?6, recipe_hash = ?7,
             size_mtime_seen = ?8, awaiting_gc = 0, last_error = NULL, own_staged = 0,
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
    )?;
    if n > 0 {
        move_to_servable(conn, project_id, frame_uuid, true)?;
    }
    sp.commit()?;
    Ok(n)
}

/// The local half of a new own content version, written the moment the
/// regenerated file replaced the landed one and was seeded — BEFORE the hub
/// confirms the version (final-review I2). Only the columns that describe the
/// file on disk move (`xxh3`, `byte_size`, `size_mtime_seen`, `on_disk = 1`,
/// `awaiting_gc = 0`), so disk truth sees the new file as present instead of
/// "edited". The hub-confirmed columns (`content_version`, `blake3`) stay
/// until [`set_own_version`] (the row is marked `own_staged` meanwhile, so the
/// collab serve check never serves the old hash from the new file), and
/// `recipe_hash` is cleared: an empty recipe
/// matches no current recipe, so ANY later publish run — not only a
/// republish — regenerates the frame, finds bytes the hub does not have, and
/// posts the version a dead run never confirmed (a republish or a plate solve
/// moves the bytes without moving the recipe). [`set_own_version`] writes the
/// recipe back. Only a row still landed at `landed_path` is touched. The row
/// is `own_held` afterwards (the file is on disk — the plan's `NewVersion`
/// edge keeps an own frame's state), moved through [`set_local_state`] in one
/// savepoint with the column write (T1/T6 ruling). Returns the rows touched.
pub fn stage_own_file(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    landed_path: &str,
    xxh3: &str,
    byte_size: i64,
    size_mtime_seen: Option<&str>,
) -> Result<usize> {
    let sp = crate::db::operations::SavepointGuard::new(conn, "stage_own_file")?;
    let n = conn.execute(
        "UPDATE project_frames_local
         SET xxh3 = ?4, byte_size = ?5, size_mtime_seen = ?6, awaiting_gc = 0,
             recipe_hash = NULL, own_staged = 1, updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2 AND origin = 'own' AND landed_path = ?3",
        params![
            project_id,
            frame_uuid,
            landed_path,
            xxh3,
            byte_size,
            size_mtime_seen
        ],
    )?;
    if n > 0 {
        set_local_state(conn, project_id, frame_uuid, LocalState::OwnHeld)?;
    }
    sp.commit()?;
    Ok(n)
}

/// Clear the `own_staged` mark of an own row whose CONFIRMED version is
/// exactly the staged file: its `blake3` is `blake3` (the hash just verified
/// against the hub) and its `xxh3` is `xxh3` (the bytes on disk). Used by the
/// publish run's identical-pixels shortcut (P19), which never goes through
/// [`set_own_version`]. Returns the rows touched.
pub fn clear_own_staged(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    blake3: &str,
    xxh3: &str,
) -> Result<usize> {
    let n = conn.execute(
        "UPDATE project_frames_local SET own_staged = 0, updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2 AND origin = 'own' AND own_staged = 1
           AND blake3 = ?3 AND xxh3 = ?4",
        params![project_id, frame_uuid, blake3, xxh3],
    )?;
    if n > 0 {
        tracing::debug!(project_id, frame_uuid, "staged own frame confirmed");
    }
    Ok(n)
}

/// Undo [`stage_own_file`] after the hub refused the new version: the file
/// at the landed path now holds bytes the hub never took, so the row goes
/// back to the hub's content keys and is marked NOT on disk (the old version
/// is gone). Disk truth then rejects the file (its size / xxh3 no longer
/// match), and the next publish run regenerates and posts again (the recipe
/// [`stage_own_file`] cleared stays cleared, so a plain run does). The row
/// moves to `own_missing` through [`set_local_state`] in one savepoint with
/// the column write (T1/T6 ruling): a servable row's claim leaves the claim
/// set through the outbox. Returns the rows touched.
pub fn unstage_own_file(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    hub_xxh3: &str,
    hub_byte_size: i64,
) -> Result<usize> {
    let sp = crate::db::operations::SavepointGuard::new(conn, "unstage_own_file")?;
    let n = conn.execute(
        "UPDATE project_frames_local
         SET xxh3 = ?3, byte_size = ?4, size_mtime_seen = NULL, awaiting_gc = 0,
             own_staged = 0, updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2 AND origin = 'own'",
        params![project_id, frame_uuid, hub_xxh3, hub_byte_size],
    )?;
    if n > 0 {
        set_local_state(conn, project_id, frame_uuid, LocalState::OwnMissing)?;
    }
    sp.commit()?;
    Ok(n)
}

/// Bind an own frame the hub already knows (a manifest-delivered row, or one
/// the hub refused as "already announced") to the local frame it was
/// generated from: `source_frame_id`, `landed_path`, `recipe_hash`,
/// `size_mtime_seen`, and `own_held` through [`set_local_state`] (one
/// savepoint with the column write, T1/T6 ruling — a manifest-first
/// `own_missing` row gets its outbox `add`). Column-targeted — hub-owned
/// columns survive. Returns the rows touched.
pub fn adopt_own(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    source_frame_id: i64,
    landed_path: &str,
    recipe_hash: &str,
    size_mtime_seen: Option<&str>,
) -> Result<usize> {
    let sp = crate::db::operations::SavepointGuard::new(conn, "adopt_own")?;
    let n = conn.execute(
        "UPDATE project_frames_local
         SET source_frame_id = ?3, landed_path = ?4, recipe_hash = ?5, size_mtime_seen = ?6,
             awaiting_gc = 0, last_error = NULL, own_staged = 0, updated_at = datetime('now')
         WHERE project_id = ?1 AND frame_uuid = ?2 AND origin = 'own'",
        params![
            project_id,
            frame_uuid,
            source_frame_id,
            landed_path,
            recipe_hash,
            size_mtime_seen
        ],
    )?;
    if n > 0 {
        set_local_state(conn, project_id, frame_uuid, LocalState::OwnHeld)?;
    }
    sp.commit()?;
    Ok(n)
}

/// Move a row to its origin's servable state (`own_held` / `held`) through
/// [`set_local_state`]. `own_only` refuses to touch a replica row (the
/// own-frame writers are never meant for one) — logged, not an error.
fn move_to_servable(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    own_only: bool,
) -> Result<()> {
    let origin: Option<String> = conn
        .query_row(
            "SELECT origin FROM project_frames_local WHERE project_id = ?1 AND frame_uuid = ?2",
            params![project_id, frame_uuid],
            |r| r.get(0),
        )
        .optional()?;
    let to = match origin.as_deref().map(FrameOrigin::from_db_str) {
        None => return Ok(()),
        Some(FrameOrigin::Own) => LocalState::OwnHeld,
        Some(FrameOrigin::Replica) if own_only => {
            tracing::warn!(
                project_id,
                frame_uuid,
                "own-frame write on a replica row; local state left alone"
            );
            return Ok(());
        }
        Some(FrameOrigin::Replica) => LocalState::Held,
    };
    set_local_state(conn, project_id, frame_uuid, to)?;
    Ok(())
}

/// Make sure a servable row's CURRENT content version is in this device's
/// claim set, through the outbox when it is not (collision C24: in the
/// caller's transaction). The publish path's adoptions need it (R8a/R8b): the
/// hub may hold no claim of this device for a frame another device of the
/// same account announced, and an adopted row that was already `own_held`
/// makes no servability flip. Returns the journal sequence written, `None`
/// when the claim set already agreed (or the row is not servable / absent).
pub fn ensure_claimed(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
) -> Result<Option<i64>> {
    let Some((state, cv)): Option<(Option<String>, i32)> = conn
        .query_row(
            "SELECT local_state, content_version FROM project_frames_local WHERE project_id = ?1 AND frame_uuid = ?2",
            params![project_id, frame_uuid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    if !LocalState::from_db_str(state.as_deref().unwrap_or("wanted")).servable() {
        return Ok(None);
    }
    let claimed: Option<i32> = conn
        .query_row(
            "SELECT content_version FROM collab_my_claims WHERE project_id = ?1 AND frame_uuid = ?2",
            params![project_id, frame_uuid],
            |r| r.get(0),
        )
        .optional()?;
    if claimed == Some(cv) {
        return Ok(None);
    }
    Ok(Some(crate::db::collab_live::record_claim_change(
        conn,
        project_id,
        frame_uuid,
        crate::db::collab_live::ClaimOp::Add {
            content_version: cv,
        },
    )?))
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

/// The foreign files listed for a project — those whose `ATH_PRJ` stamp
/// names it, plus the unstamped ones (they belong to no project, so every
/// project's "Other files" list shows them). `(path, seen_at)` by path.
pub fn list_foreign_files(conn: &Connection, project_id: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT path, seen_at FROM collab_foreign_files
         WHERE project_id = ?1 OR project_id IS NULL ORDER BY path",
    )?;
    let rows = stmt
        .query_map(params![project_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(String, String)>>>()?;
    Ok(rows)
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
    fn foreign_files_list_the_projects_own_and_the_unstamped() {
        let c = conn();
        record_foreign_file(&c, "/collab/b.fits", Some("p1"), None).unwrap();
        record_foreign_file(&c, "/collab/a.fits", None, None).unwrap();
        record_foreign_file(&c, "/collab/c.fits", Some("p2"), None).unwrap();
        let paths: Vec<String> = list_foreign_files(&c, "p1")
            .unwrap()
            .into_iter()
            .map(|(p, seen)| {
                assert!(!seen.is_empty());
                p
            })
            .collect();
        assert_eq!(paths, vec!["/collab/a.fits", "/collab/b.fits"]);
    }

    // ── Amendment A6: own per device ─────────────────────────────────────

    /// A row the pre-A6 derivation cached as own (another device of this
    /// account published it — `own_missing`, no file here) becomes a
    /// `wanted` replica once the manifest names the other device: the own
    /// state never survives on a replica, and it is fetched like any other.
    #[test]
    fn an_own_row_of_another_device_becomes_a_wanted_replica() {
        let c = conn();
        let mut v = view("u1", 1);
        v.own = true;
        upsert_from_manifest(&c, "p1", &v).unwrap();
        c.execute(
            "UPDATE project_frames_local SET source_frame_id = 7, recipe_hash = 'r'
             WHERE frame_uuid = 'u1'",
            [],
        )
        .unwrap();
        assert_eq!(
            get(&c, "p1", "u1").unwrap().unwrap().local_state,
            LocalState::OwnMissing
        );
        v.own = false;
        v.manifest_version = 2;
        upsert_from_manifest(&c, "p1", &v).unwrap();
        let row = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(row.origin, FrameOrigin::Replica);
        assert_eq!(row.local_state, LocalState::Wanted);
        assert_eq!((row.source_frame_id, row.recipe_hash), (None, None));

        // Unpublished (or excluded) → idle, not wanted.
        let mut p = view("u2", 1);
        p.own = true;
        upsert_from_manifest(&c, "p1", &p).unwrap();
        p.own = false;
        p.accepted = false;
        upsert_from_manifest(&c, "p1", &p).unwrap();
        assert_eq!(
            get(&c, "p1", "u2").unwrap().unwrap().local_state,
            LocalState::Idle
        );
    }

    /// An `own_held` row turned replica stops being claimed (its servable
    /// state is gone until the storage engine re-adopts the file by hash).
    #[test]
    fn an_own_held_row_turned_replica_drops_its_claim() {
        let c = conn();
        let mut v = view("u1", 1);
        v.own = true;
        upsert_from_manifest(&c, "p1", &v).unwrap();
        set_local_state(&c, "p1", "u1", LocalState::OwnHeld).unwrap();
        assert!(crate::db::collab_live::my_claims(&c, "p1")
            .unwrap()
            .iter()
            .any(|(u, _)| u == "u1"));
        v.own = false;
        upsert_from_manifest(&c, "p1", &v).unwrap();
        let row = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(
            (row.origin, row.local_state),
            (FrameOrigin::Replica, LocalState::Wanted)
        );
        assert!(!row.on_disk);
        assert!(!crate::db::collab_live::my_claims(&c, "p1")
            .unwrap()
            .iter()
            .any(|(u, _)| u == "u1"));
    }

    /// The reverse (this device's key is the publisher): a held replica
    /// becomes `own_held`, anything else `own_missing`.
    #[test]
    fn a_replica_this_device_published_becomes_own() {
        let c = conn();
        let v = view("u1", 1);
        upsert_from_manifest(&c, "p1", &v).unwrap();
        set_local_state(&c, "p1", "u1", LocalState::Held).unwrap();
        let mut mine = v.clone();
        mine.own = true;
        upsert_from_manifest(&c, "p1", &mine).unwrap();
        let row = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(
            (row.origin, row.local_state),
            (FrameOrigin::Own, LocalState::OwnHeld)
        );

        upsert_from_manifest(&c, "p1", &view("u2", 1)).unwrap();
        let mut mine2 = view("u2", 2);
        mine2.own = true;
        upsert_from_manifest(&c, "p1", &mine2).unwrap();
        assert_eq!(
            get(&c, "p1", "u2").unwrap().unwrap().local_state,
            LocalState::OwnMissing
        );
    }

    /// Final fix B-M1: a QUARANTINED replica this device published turns
    /// own as `own_changed` (its changed file is not served), and its
    /// quarantine record goes in the same write — never an orphaned
    /// "Changed files" record nothing can resolve.
    #[test]
    fn a_quarantined_replica_this_device_published_becomes_own_changed() {
        let c = conn();
        let v = view("u1", 1);
        upsert_from_manifest(&c, "p1", &v).unwrap();
        set_local_state(&c, "p1", "u1", LocalState::Quarantined).unwrap();
        crate::db::collab_live::quarantine(
            &c,
            &crate::db::collab_live::QuarantineRow {
                project_id: "p1".into(),
                frame_uuid: "u1".into(),
                path: "/collab/m31/ann/c_u1.fits".into(),
                detected_at: String::new(),
                quarantined_version: 1,
                observed_size_mtime: Some("100:1700000000".into()),
            },
        )
        .unwrap();
        let mut mine = v.clone();
        mine.own = true;
        upsert_from_manifest(&c, "p1", &mine).unwrap();
        let row = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(
            (row.origin, row.local_state),
            (FrameOrigin::Own, LocalState::OwnChanged)
        );
        assert!(crate::db::collab_live::list_quarantine(&c, "p1")
            .unwrap()
            .is_empty());
    }

    /// Every name a publisher has in the project's manifest, whatever the
    /// device or origin — and only that publisher's.
    #[test]
    fn file_names_of_publisher_cover_the_whole_manifest() {
        let c = conn();
        let mut own = view("u1", 1);
        own.own = true;
        upsert_from_manifest(&c, "p1", &own).unwrap();
        upsert_from_manifest(&c, "p1", &view("u2", 1)).unwrap();
        let mut other = view("u3", 1);
        other.publisher_account_id = "a2".into();
        upsert_from_manifest(&c, "p1", &other).unwrap();
        let names = file_names_of_publisher(&c, "p1", "a1").unwrap();
        let mut names: Vec<String> = names.into_iter().collect();
        names.sort();
        assert_eq!(names, vec!["c_u1.fits", "c_u2.fits"]);
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
        // Task 9 (spec §9.4): an excluded frame goes `idle` — the file and
        // its recorded path are kept, but it is no longer served.
        assert!(!r.on_disk && !r.accepted);
        assert_eq!(r.local_state, LocalState::Idle);
        assert_eq!(r.landed_path.as_deref(), Some("/collab/m31/ann/c_u1.fits"));
        assert_eq!(r.size_mtime_seen.as_deref(), Some("100:1700000000"));
        assert_eq!(r.manifest_version, 2);
    }

    fn outbox_ops(c: &Connection) -> Vec<(String, crate::db::collab_live::ClaimOp)> {
        crate::db::collab_live::outbox(c, "p1")
            .unwrap()
            .into_iter()
            .map(|r| (r.frame_uuid, r.op))
            .collect()
    }

    fn held(c: &Connection, uuid: &str, path: &str) {
        upsert_from_manifest(c, "p1", &view(uuid, 1)).unwrap();
        update_landed_path(c, "p1", uuid, path).unwrap();
        set_size_mtime_seen(c, "p1", uuid, "100:1").unwrap();
        set_local_state(c, "p1", uuid, LocalState::Held).unwrap();
        crate::db::collab_live::ack_outbox(c, "p1", i64::MAX).unwrap();
    }

    fn own_staged(c: &Connection, uuid: &str) -> bool {
        c.query_row(
            "SELECT own_staged FROM project_frames_local WHERE project_id = 'p1' AND frame_uuid = ?1",
            [uuid],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Task 10 fix round 1 (C11): `own_staged` is set by `stage_own_file` and
    /// cleared by `set_own_version`, `unstage_own_file` and `adopt_own`; the
    /// one-time backfill marks an on-disk own row without a recipe (the only
    /// shape `stage_own_file` left before wave 3).
    #[test]
    fn own_staged_follows_the_own_version_writers_and_is_backfilled() {
        let c = conn();
        let mut own = view("u1", 1);
        own.own = true;
        upsert_from_manifest(&c, "p1", &own).unwrap();
        adopt_own(&c, "p1", "u1", 7, "/c/u1.fits", "r1", Some("100:1")).unwrap();
        assert!(!own_staged(&c, "u1"));
        stage_own_file(
            &c,
            "p1",
            "u1",
            "/c/u1.fits",
            "fedcba9876543210",
            120,
            Some("120:2"),
        )
        .unwrap();
        assert!(own_staged(&c, "u1"));
        set_own_version(
            &c,
            "p1",
            "u1",
            2,
            &"c".repeat(64),
            "fedcba9876543210",
            120,
            "r2",
            Some("120:2"),
        )
        .unwrap();
        assert!(!own_staged(&c, "u1"));
        stage_own_file(
            &c,
            "p1",
            "u1",
            "/c/u1.fits",
            "0000000000000000",
            130,
            Some("130:3"),
        )
        .unwrap();
        unstage_own_file(&c, "p1", "u1", "fedcba9876543210", 120).unwrap();
        assert!(!own_staged(&c, "u1"));
        stage_own_file(
            &c,
            "p1",
            "u1",
            "/c/u1.fits",
            "0000000000000000",
            130,
            Some("130:3"),
        )
        .unwrap();
        adopt_own(&c, "p1", "u1", 7, "/c/u1.fits", "r3", Some("130:3")).unwrap();
        assert!(!own_staged(&c, "u1"));

        // The backfill: drop the column, shape a pre-wave-3 staged row and a
        // published one, and let init_db add the column again.
        let mut other = view("u2", 1);
        other.own = true;
        upsert_from_manifest(&c, "p1", &other).unwrap();
        adopt_own(&c, "p1", "u2", 8, "/c/u2.fits", "r1", Some("100:1")).unwrap();
        c.execute_batch(
            "UPDATE project_frames_local SET recipe_hash = NULL, on_disk = 1 WHERE frame_uuid = 'u1';
             ALTER TABLE project_frames_local DROP COLUMN own_staged;",
        )
        .unwrap();
        crate::db::schema::init_db(&c).unwrap();
        assert!(own_staged(&c, "u1"), "a pre-wave-3 staged row");
        assert!(!own_staged(&c, "u2"), "a published row");
    }

    /// Task 9 step 4: the manifest's full edge set.
    #[test]
    fn a_new_version_of_a_held_replica_is_wanted_again_and_its_claim_dropped() {
        use crate::db::collab_live::ClaimOp;
        let c = conn();
        held(&c, "u1", "/c/m31/ann/u1.fits");
        let mut v2 = view("u1", 2);
        v2.content_version = 2;
        v2.blake3 = "c".repeat(64);
        upsert_from_manifest(&c, "p1", &v2).unwrap();
        let r = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(r.local_state, LocalState::Wanted);
        assert!(!r.on_disk && r.size_mtime_seen.is_none());
        assert_eq!(
            r.landed_path.as_deref(),
            Some("/c/m31/ann/u1.fits"),
            "the v1 file stays until v2 replaces it (L7)"
        );
        assert_eq!(outbox_ops(&c), vec![("u1".to_string(), ClaimOp::Remove)]);
    }

    /// Wave-3 replacement for the retired wave-2
    /// `a_same_content_version_bump_keeps_the_landed_file` (T11/T12/T18
    /// ruling): the same bytes under a new version keep the landed file, its
    /// path and its stamp, stay `held`, and are re-claimed at the new version
    /// (P10) — nothing to fetch.
    #[test]
    fn a_same_content_version_bump_keeps_the_landed_file_and_reclaims() {
        use crate::db::collab_live::ClaimOp;
        let c = conn();
        held(&c, "u1", "/c/m31/ann/u1.fits");
        let mut v2 = view("u1", 2);
        v2.content_version = 2;
        upsert_from_manifest(&c, "p1", &v2).unwrap();
        let r = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(r.local_state, LocalState::Held);
        assert!(r.on_disk);
        assert_eq!(r.content_version, 2);
        assert_eq!(r.landed_path.as_deref(), Some("/c/m31/ann/u1.fits"));
        assert_eq!(r.size_mtime_seen.as_deref(), Some("100:1"));
        assert_eq!(
            outbox_ops(&c),
            vec![("u1".to_string(), ClaimOp::Add { content_version: 2 })]
        );
    }

    #[test]
    fn a_new_version_leaves_quarantined_not_kept_and_awaiting_rows_alone() {
        let c = conn();
        for (uuid, st) in [
            ("q", LocalState::Quarantined),
            ("n", LocalState::NotKept),
            ("a", LocalState::AwaitingChoice),
            ("i", LocalState::Idle),
        ] {
            held(&c, uuid, &format!("/c/m31/ann/{uuid}.fits"));
            set_local_state(&c, "p1", uuid, st).unwrap();
            crate::db::collab_live::ack_outbox(&c, "p1", i64::MAX).unwrap();
            let mut v2 = view(uuid, 2);
            v2.content_version = 2;
            v2.blake3 = "d".repeat(64);
            upsert_from_manifest(&c, "p1", &v2).unwrap();
            let r = get(&c, "p1", uuid).unwrap().unwrap();
            assert_eq!(r.local_state, st, "{uuid}");
            if st == LocalState::Quarantined {
                assert_eq!(
                    r.size_mtime_seen.as_deref(),
                    Some("100:1"),
                    "a quarantined file is the user's to resolve"
                );
            }
        }
        assert!(outbox_ops(&c).is_empty());
    }

    #[test]
    fn exclusion_idles_a_held_replica_and_reinclusion_decides_by_stat() {
        use crate::db::collab_live::ClaimOp;
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("u1.fits");
        std::fs::write(&f, vec![7u8; 100]).unwrap();
        let stamp = crate::collab::storage::sweep::Stamp::of(&std::fs::metadata(&f).unwrap());
        let c = conn();
        held(&c, "u1", &f.to_string_lossy());
        set_size_mtime_seen(&c, "p1", "u1", &stamp.encode()).unwrap();

        let mut out = view("u1", 2);
        out.accepted = false;
        upsert_from_manifest(&c, "p1", &out).unwrap();
        assert_eq!(
            get(&c, "p1", "u1").unwrap().unwrap().local_state,
            LocalState::Idle
        );
        assert_eq!(outbox_ops(&c), vec![("u1".to_string(), ClaimOp::Remove)]);
        crate::db::collab_live::ack_outbox(&c, "p1", i64::MAX).unwrap();

        // re-included with the file untouched → held again, re-claimed
        upsert_from_manifest(&c, "p1", &view("u1", 3)).unwrap();
        let r = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(r.local_state, LocalState::Held);
        assert_eq!(
            outbox_ops(&c),
            vec![("u1".to_string(), ClaimOp::Add { content_version: 1 })]
        );

        // excluded again, the file removed meanwhile → re-included as wanted
        upsert_from_manifest(&c, "p1", &out).unwrap();
        std::fs::remove_file(&f).unwrap();
        upsert_from_manifest(&c, "p1", &view("u1", 4)).unwrap();
        assert_eq!(
            get(&c, "p1", "u1").unwrap().unwrap().local_state,
            LocalState::Wanted
        );

        // an idle row whose path was only recorded (no verified stamp) is
        // wanted, never held
        let mut idle = view("u2", 1);
        idle.accepted = false;
        upsert_from_manifest(&c, "p1", &idle).unwrap();
        let g = tmp.path().join("u2.fits");
        std::fs::write(&g, vec![7u8; 100]).unwrap();
        update_landed_path(&c, "p1", "u2", &g.to_string_lossy()).unwrap();
        upsert_from_manifest(&c, "p1", &view("u2", 2)).unwrap();
        assert_eq!(
            get(&c, "p1", "u2").unwrap().unwrap().local_state,
            LocalState::Wanted
        );
    }

    /// T9 review fold (Task 10): inside a transaction the re-included
    /// frame's file is collected, not routed — the caller routes it after
    /// its commit, so a rolled-back upsert hands nothing to the engine.
    #[test]
    fn a_deferred_upsert_collects_the_engine_route_for_after_the_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("u2.fits");
        let c = conn();
        let mut idle = view("u2", 1);
        idle.accepted = false;
        upsert_from_manifest(&c, "p1", &idle).unwrap();
        std::fs::write(&f, vec![7u8; 100]).unwrap();
        update_landed_path(&c, "p1", "u2", &f.to_string_lossy()).unwrap();

        let tx = c.unchecked_transaction().unwrap();
        let mut routes = EngineRoutes::default();
        upsert_from_manifest_deferred(&tx, "p1", &view("u2", 2), &mut routes).unwrap();
        assert_eq!(
            routes.0,
            vec![("p1".to_string(), "u2".to_string(), f.clone())],
            "the landed file is collected for the engine"
        );
        drop(tx); // rolled back
        assert_eq!(
            get(&c, "p1", "u2").unwrap().unwrap().local_state,
            LocalState::Idle,
            "the rollback undid the re-inclusion; the route was never sent"
        );
    }

    /// Fix round 1: a parked row (`awaiting_gc`) never stays parked across a
    /// new version or a re-inclusion, and a parked or not-seeded row is
    /// never held on a stat alone.
    #[test]
    fn parked_rows_are_released_by_a_new_version_or_a_reinclusion() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("u1.fits");
        std::fs::write(&f, vec![7u8; 100]).unwrap();
        let stamp = crate::collab::storage::sweep::Stamp::of(&std::fs::metadata(&f).unwrap());
        let park = |c: &Connection, uuid: &str| {
            c.execute(
                "UPDATE project_frames_local SET awaiting_gc = 1, size_mtime_seen = ?2 WHERE frame_uuid = ?1",
                params![uuid, stamp.encode()],
            )
            .unwrap();
        };
        let c = conn();
        held(&c, "u1", &f.to_string_lossy());
        set_local_state(&c, "p1", "u1", LocalState::Wanted).unwrap();
        park(&c, "u1");
        assert_eq!(parked_rows(&c).unwrap().len(), 1);

        // a new version (new bytes) releases it
        let mut v2 = view("u1", 2);
        v2.content_version = 2;
        v2.blake3 = "c".repeat(64);
        upsert_from_manifest(&c, "p1", &v2).unwrap();
        assert!(!get(&c, "p1", "u1").unwrap().unwrap().awaiting_gc);

        // parked, excluded, re-included with a same-stat file: wanted, not
        // held (it is not known to be seeded), and no longer parked
        park(&c, "u1");
        let mut out = v2.clone();
        out.accepted = false;
        upsert_from_manifest(&c, "p1", &out).unwrap();
        assert_eq!(
            get(&c, "p1", "u1").unwrap().unwrap().local_state,
            LocalState::Idle
        );
        park(&c, "u1");
        upsert_from_manifest(&c, "p1", &v2).unwrap();
        let r = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(r.local_state, LocalState::Wanted);
        assert!(!r.awaiting_gc);
    }

    #[test]
    fn an_exclusion_from_a_non_servable_state_drops_the_seeded_record() {
        let c = conn();
        held(&c, "u1", "/c/m31/ann/u1.fits");
        set_local_state(&c, "p1", "u1", LocalState::Wanted).unwrap();
        let mut out = view("u1", 2);
        out.accepted = false;
        upsert_from_manifest(&c, "p1", &out).unwrap();
        let r = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(r.local_state, LocalState::Idle);
        assert!(r.size_mtime_seen.is_none(), "a wanted row was never seeded");
        // from held: seeded, the stamp stays
        held(&c, "u2", "/c/m31/ann/u2.fits");
        let mut out = view("u2", 2);
        out.accepted = false;
        upsert_from_manifest(&c, "p1", &out).unwrap();
        assert!(get(&c, "p1", "u2")
            .unwrap()
            .unwrap()
            .size_mtime_seen
            .is_some());
    }

    #[test]
    fn rows_under_is_separator_strict_and_list_by_state_filters() {
        let c = conn();
        held(&c, "a", "/c/m31/ann/a.fits");
        held(&c, "b", "/c/m31/ann/sub/b.fits");
        held(&c, "c", "/c/m31/ann_x/c.fits");
        upsert_from_manifest(&c, "p1", &view("d", 1)).unwrap();
        let under: Vec<String> = rows_under(&c, "/c/m31/ann")
            .unwrap()
            .into_iter()
            .map(|r| r.frame_uuid)
            .collect();
        assert_eq!(under, vec!["a", "b"]);
        assert_eq!(rows_under(&c, "/c/m31/ann/a.fits").unwrap().len(), 1);
        assert_eq!(rows_with_landed_path(&c).unwrap().len(), 3);
        set_local_state(&c, "p1", "c", LocalState::NotKept).unwrap();
        let listed: Vec<String> =
            list_by_state(&c, "p1", &[LocalState::NotKept, LocalState::Wanted])
                .unwrap()
                .into_iter()
                .map(|r| r.frame_uuid)
                .collect();
        assert_eq!(listed, vec!["c", "d"]);
        assert!(list_by_state(&c, "p1", &[]).unwrap().is_empty());
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
            publisher_dir(&c, "p1", "a1", Path::new("/collab")).unwrap(),
            Some(PathBuf::from("/collab/m31/ann"))
        );
        assert_eq!(
            publisher_dir(&c, "p1", "zz", Path::new("/collab")).unwrap(),
            None
        );
        // a landing under a previous Collaboration folder never counts
        assert_eq!(
            publisher_dir(&c, "p1", "a1", Path::new("/collab2")).unwrap(),
            None
        );
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
        // new bytes (a same-bytes bump keeps the file — Task 9, P10)
        bumped.blake3 = "c".repeat(64);
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

    /// R20 + Task 11 (P13): `set_landed_if` lands only on the version and
    /// content it was asked for, and only on a `wanted` row; a moved or
    /// quarantined row stays untouched. It records the path and stamp only
    /// — the caller's `set_local_state(Held)` in the same transaction moves
    /// `on_disk`/`local_state` and appends the outbox `add`.
    #[test]
    fn set_landed_if_refuses_a_moved_version() {
        let c = conn();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        let r = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(r.local_state, LocalState::Wanted);
        let land =
            |cv: i32, b3: &str| set_landed_if(&c, "p1", "u1", "/x/a.fits", "1:1", cv, b3).unwrap();
        assert_eq!(land(r.content_version + 1, &r.blake3), 0);
        assert_eq!(land(r.content_version, "f00d"), 0);
        assert!(!get(&c, "p1", "u1").unwrap().unwrap().on_disk);

        // P13: not `wanted` (a quarantined file) — refused on the same
        // version and content.
        set_local_state(&c, "p1", "u1", LocalState::Quarantined).unwrap();
        assert_eq!(land(r.content_version, &r.blake3), 0);
        let q = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(q.local_state, LocalState::Quarantined);
        assert!(q.landed_path.is_none());
        set_local_state(&c, "p1", "u1", LocalState::Wanted).unwrap();

        set_rejected_size_mtime(&c, "p1", "u1", "9:9").unwrap();
        assert_eq!(
            rejected_size_mtime(&c, "p1", "u1").unwrap().as_deref(),
            Some("9:9")
        );
        let tx = c.unchecked_transaction().unwrap();
        assert_eq!(
            set_landed_if(
                &tx,
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
        let recorded = get(&tx, "p1", "u1").unwrap().unwrap();
        assert_eq!(recorded.landed_path.as_deref(), Some("/x/a.fits"));
        assert_eq!(
            (recorded.local_state, recorded.on_disk),
            (LocalState::Wanted, false),
            "the state is the caller's set_local_state"
        );
        let w = set_local_state(&tx, "p1", "u1", LocalState::Held)
            .unwrap()
            .unwrap();
        assert_eq!(
            w.claim,
            Some(crate::db::collab_live::ClaimOp::Add { content_version: 1 })
        );
        tx.commit().unwrap();
        let landed = get(&c, "p1", "u1").unwrap().unwrap();
        assert!(landed.on_disk);
        assert_eq!(landed.local_state, LocalState::Held);
        assert_eq!(landed.size_mtime_seen.as_deref(), Some("1:1"));
        assert_eq!(
            rejected_size_mtime(&c, "p1", "u1").unwrap(),
            None,
            "a landing clears it"
        );
    }

    /// Task 11 fix rounds 1+2 (L5/C38): a new version keeps the verified
    /// stamp as `prev_stamp` on a `wanted` AND an `idle` row (the FIRST one
    /// across further bumps while still wanted); `wanted → idle → wanted`
    /// keeps it; only a newer truth clears it — entering `held`, `own_held`
    /// or `quarantined`, or a recorded landing.
    #[test]
    fn a_new_version_keeps_the_previous_stamp_until_a_newer_truth_replaces_it() {
        let c = conn();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        let r = get(&c, "p1", "u1").unwrap().unwrap();
        set_landed_if(&c, "p1", "u1", "/x/a.fits", "100:7", 1, &r.blake3).unwrap();
        set_local_state(&c, "p1", "u1", LocalState::Held).unwrap();
        assert_eq!(prev_stamp(&c, "p1", "u1").unwrap(), None);

        let bump = |cv: i32, b: &str| {
            let mut v = view("u1", cv as i64);
            v.content_version = cv;
            v.blake3 = b.repeat(64);
            upsert_from_manifest(&c, "p1", &v).unwrap();
        };
        bump(2, "c");
        let r = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(r.local_state, LocalState::Wanted);
        assert_eq!(r.size_mtime_seen, None);
        assert_eq!(
            prev_stamp(&c, "p1", "u1").unwrap().as_deref(),
            Some("100:7")
        );
        bump(3, "d");
        assert_eq!(
            prev_stamp(&c, "p1", "u1").unwrap().as_deref(),
            Some("100:7"),
            "the file on disk is still the one verified first"
        );
        // wanted → idle → wanted keeps it (an exclusion is not a newer truth)
        set_local_state(&c, "p1", "u1", LocalState::Idle).unwrap();
        assert_eq!(
            prev_stamp(&c, "p1", "u1").unwrap().as_deref(),
            Some("100:7"),
            "idle keeps it"
        );
        set_local_state(&c, "p1", "u1", LocalState::Wanted).unwrap();
        assert_eq!(
            prev_stamp(&c, "p1", "u1").unwrap().as_deref(),
            Some("100:7"),
            "re-included: still there for the pre-bump check"
        );
        // entering held clears it
        set_local_state(&c, "p1", "u1", LocalState::Held).unwrap();
        assert_eq!(prev_stamp(&c, "p1", "u1").unwrap(), None, "entered held");

        // an idle row keeps its stamp across a bump (I-1): held v3 verified
        // at 100:8, excluded (the stamp stays), v4 released while idle
        c.execute(
            "UPDATE project_frames_local SET size_mtime_seen = '100:8' WHERE frame_uuid = 'u1'",
            [],
        )
        .unwrap();
        set_local_state(&c, "p1", "u1", LocalState::Idle).unwrap();
        bump(4, "e");
        let r = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(r.local_state, LocalState::Idle);
        assert_eq!(r.size_mtime_seen, None);
        assert_eq!(
            prev_stamp(&c, "p1", "u1").unwrap().as_deref(),
            Some("100:8"),
            "an idle row keeps the stamp a bump clears"
        );
        set_local_state(&c, "p1", "u1", LocalState::Wanted).unwrap();
        assert_eq!(
            prev_stamp(&c, "p1", "u1").unwrap().as_deref(),
            Some("100:8")
        );
        // entering quarantined clears it
        set_local_state(&c, "p1", "u1", LocalState::Quarantined).unwrap();
        assert_eq!(
            prev_stamp(&c, "p1", "u1").unwrap(),
            None,
            "entered quarantined"
        );
        // entering own_held clears it (the SQL rule; own rows never bump here)
        c.execute(
            "UPDATE project_frames_local SET prev_stamp = '1:2' WHERE frame_uuid = 'u1'",
            [],
        )
        .unwrap();
        set_local_state(&c, "p1", "u1", LocalState::OwnHeld).unwrap();
        assert_eq!(
            prev_stamp(&c, "p1", "u1").unwrap(),
            None,
            "entered own_held"
        );

        set_local_state(&c, "p1", "u1", LocalState::Wanted).unwrap();
        c.execute(
            "UPDATE project_frames_local SET prev_stamp = '1:1' WHERE frame_uuid = 'u1'",
            [],
        )
        .unwrap();
        set_landed_if(&c, "p1", "u1", "/x/a.fits", "100:9", 4, &"e".repeat(64)).unwrap();
        assert_eq!(
            prev_stamp(&c, "p1", "u1").unwrap(),
            None,
            "a landing clears it"
        );
    }

    /// Task 11 fix round 2 (M-a): a re-admitted version (`set_landed`) must
    /// not carry an older version's stamp into the next bump.
    #[test]
    fn set_landed_clears_a_stale_previous_stamp() {
        let c = conn();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        set_local_state(&c, "p1", "u1", LocalState::Held).unwrap();
        c.execute(
            "UPDATE project_frames_local SET prev_stamp = '100:7' WHERE frame_uuid = 'u1'",
            [],
        )
        .unwrap();
        set_landed(&c, "p1", "u1", "/x/a.fits", "100:9").unwrap();
        let r = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(r.local_state, LocalState::Held);
        assert_eq!(r.size_mtime_seen.as_deref(), Some("100:9"));
        assert_eq!(prev_stamp(&c, "p1", "u1").unwrap(), None);
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

    fn own_manifest_row(c: &Connection, uuid: &str) {
        let mut v = view(uuid, 1);
        v.own = true;
        upsert_from_manifest(c, "p1", &v).unwrap();
    }

    fn state_of(c: &Connection, uuid: &str) -> (LocalState, bool) {
        let r = get(c, "p1", uuid).unwrap().unwrap();
        (r.local_state, r.on_disk)
    }

    /// T1/T6 ruling: every publish-path writer of an own row's disk state
    /// moves it through `set_local_state` — `on_disk` follows the state, and a
    /// servability flip writes its claim + outbox row in the same savepoint.
    #[test]
    fn publish_writers_move_own_rows_through_the_state_machine() {
        use crate::db::collab_live::{my_claims, outbox, outbox_len, ClaimOp};
        let c = conn();
        own_manifest_row(&c, "u1");
        assert_eq!(state_of(&c, "u1"), (LocalState::OwnMissing, false));

        // adopt: own_missing → own_held, outbox add of the current version
        assert_eq!(
            adopt_own(&c, "p1", "u1", 7, "/c/u1.fits", "r0", Some("1:2")).unwrap(),
            1
        );
        assert_eq!(state_of(&c, "u1"), (LocalState::OwnHeld, true));
        assert_eq!(my_claims(&c, "p1").unwrap(), vec![("u1".to_string(), 1)]);
        assert_eq!(outbox_len(&c, "p1").unwrap(), 1);

        // stage: stays own_held, no claim change
        assert_eq!(
            stage_own_file(
                &c,
                "p1",
                "u1",
                "/c/u1.fits",
                "0123456789abcdee",
                101,
                Some("101:3")
            )
            .unwrap(),
            1
        );
        assert_eq!(state_of(&c, "u1"), (LocalState::OwnHeld, true));
        assert_eq!(outbox_len(&c, "p1").unwrap(), 1);
        // a stage at another path touches nothing
        assert_eq!(
            stage_own_file(&c, "p1", "u1", "/elsewhere.fits", "x", 1, None).unwrap(),
            0
        );

        // set_own_version on a held row: no flip, no outbox row (the new
        // version's claim is the hub's implicit one, the caller's to add)
        assert_eq!(
            set_own_version(
                &c,
                "p1",
                "u1",
                2,
                &"c".repeat(64),
                "0123456789abcdee",
                101,
                "r1",
                Some("101:3")
            )
            .unwrap(),
            1
        );
        assert_eq!(state_of(&c, "u1"), (LocalState::OwnHeld, true));
        assert_eq!(outbox_len(&c, "p1").unwrap(), 1);

        // unstage: own_missing, the claim leaves through the outbox
        assert_eq!(
            unstage_own_file(&c, "p1", "u1", "0123456789abcdef", 100).unwrap(),
            1
        );
        assert_eq!(state_of(&c, "u1"), (LocalState::OwnMissing, false));
        assert!(my_claims(&c, "p1").unwrap().is_empty());
        let rows = outbox(&c, "p1").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].op, ClaimOp::Remove);

        // set_own_version on a missing row lifts it back with the NEW version
        set_own_version(
            &c,
            "p1",
            "u1",
            3,
            &"d".repeat(64),
            "0123456789abcdee",
            101,
            "r2",
            None,
        )
        .unwrap();
        assert_eq!(state_of(&c, "u1"), (LocalState::OwnHeld, true));
        assert_eq!(my_claims(&c, "p1").unwrap(), vec![("u1".to_string(), 3)]);

        // a replica row is never touched by the own-frame writers' state move
        upsert_from_manifest(&c, "p1", &view("u2", 1)).unwrap();
        set_own_version(
            &c,
            "p1",
            "u2",
            2,
            &"e".repeat(64),
            "0123456789abcdee",
            1,
            "r",
            None,
        )
        .unwrap();
        assert_eq!(state_of(&c, "u2").0, LocalState::Wanted);
    }

    #[test]
    fn ensure_claimed_adds_only_a_missing_or_stale_claim_of_a_servable_row() {
        use crate::db::collab_live::{add_implicit_claim, my_claims, outbox_len};
        let c = conn();
        own_manifest_row(&c, "u1");
        // not servable → nothing
        assert_eq!(ensure_claimed(&c, "p1", "u1").unwrap(), None);
        let mut row = get(&c, "p1", "u1").unwrap().unwrap();
        row.on_disk = true;
        row.landed_path = Some("/c/u1.fits".into());
        record_own(&c, &row).unwrap();
        // own_held without a claim → one outbox add
        assert!(ensure_claimed(&c, "p1", "u1").unwrap().is_some());
        assert_eq!(my_claims(&c, "p1").unwrap(), vec![("u1".to_string(), 1)]);
        assert_eq!(ensure_claimed(&c, "p1", "u1").unwrap(), None);
        // a stale version in the claim set → re-added at the row's version
        add_implicit_claim(&c, "p1", "u1", 7).unwrap();
        assert!(ensure_claimed(&c, "p1", "u1").unwrap().is_some());
        assert_eq!(my_claims(&c, "p1").unwrap(), vec![("u1".to_string(), 1)]);
        assert_eq!(outbox_len(&c, "p1").unwrap(), 2);
        assert_eq!(ensure_claimed(&c, "p1", "nope").unwrap(), None);
    }

    /// Task 18 (spec §12 "epoch rotation", no lost holdings): a claim the
    /// hub refused because a restore lost the frame is reported again when
    /// the publisher's re-announce lists the frame anew under its uuid — the
    /// row stayed `held` throughout, so nothing else would re-add it.
    #[test]
    fn a_refused_claim_is_reported_again_once_the_hub_lists_the_frame() {
        use crate::db::collab_live::{drop_refused_claim, my_claims, outbox, REFUSED_CLAIM_ERROR};
        let c = conn();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        set_local_state(&c, "p1", "u1", LocalState::Held).unwrap();
        let reported = outbox(&c, "p1").unwrap().last().unwrap().seq;
        // the report reached the hub after the restore: refused, dropped
        assert!(drop_refused_claim(&c, "p1", "u1", 1, reported).unwrap());
        set_error(&c, "p1", "u1", Some(REFUSED_CLAIM_ERROR)).unwrap();
        assert!(my_claims(&c, "p1").unwrap().is_empty());
        let before = outbox(&c, "p1").unwrap().len();
        // the publisher re-announced it: the manifest lists it again
        upsert_from_manifest(&c, "p1", &view("u1", 5)).unwrap();
        assert_eq!(my_claims(&c, "p1").unwrap(), vec![("u1".to_string(), 1)]);
        assert_eq!(outbox(&c, "p1").unwrap().len(), before + 1);
        let row = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(row.local_state, LocalState::Held);
        assert_eq!(row.last_error, None);
        // idempotent: a later upsert adds nothing
        upsert_from_manifest(&c, "p1", &view("u1", 6)).unwrap();
        assert_eq!(outbox(&c, "p1").unwrap().len(), before + 1);
    }

    /// A row whose claim was never refused is never re-claimed by an
    /// upsert (a staged own row must not claim its old version early).
    #[test]
    fn an_upsert_leaves_an_unrefused_unclaimed_row_alone() {
        use crate::db::collab_live::{my_claims, outbox};
        let c = conn();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        set_local_state(&c, "p1", "u1", LocalState::Held).unwrap();
        c.execute("DELETE FROM collab_my_claims", []).unwrap();
        let before = outbox(&c, "p1").unwrap().len();
        upsert_from_manifest(&c, "p1", &view("u1", 2)).unwrap();
        assert!(my_claims(&c, "p1").unwrap().is_empty());
        assert_eq!(outbox(&c, "p1").unwrap().len(), before);
    }

    /// T1 minor carried by Task 6: `record_own` is for new rows, and a
    /// manifest-assigned `frame_seq` survives a row argument without one.
    #[test]
    fn record_own_keeps_a_manifest_frame_seq() {
        let c = conn();
        let mut v = view("u1", 1);
        v.own = true;
        v.frame_seq = 12;
        upsert_from_manifest(&c, "p1", &v).unwrap();
        let mut row = get(&c, "p1", "u1").unwrap().unwrap();
        assert_eq!(row.frame_seq, Some(12));
        row.frame_seq = None;
        row.on_disk = true;
        record_own(&c, &row).unwrap();
        assert_eq!(get(&c, "p1", "u1").unwrap().unwrap().frame_seq, Some(12));
        // an explicit value still wins
        row.frame_seq = Some(13);
        record_own(&c, &row).unwrap();
        assert_eq!(get(&c, "p1", "u1").unwrap().unwrap().frame_seq, Some(13));
    }

    /// T1 minor carried by Task 6: the interim state writers stamp
    /// `state_changed_at` when (and only when) the state moves.
    #[test]
    fn interim_writers_stamp_state_changed_at_on_a_move() {
        let c = conn();
        upsert_from_manifest(&c, "p1", &view("u1", 1)).unwrap();
        let stamp = |c: &Connection| -> Option<String> {
            c.query_row(
                "SELECT state_changed_at FROM project_frames_local WHERE frame_uuid = 'u1'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert!(stamp(&c).is_some(), "a new manifest row is stamped");
        let reset = |c: &Connection| {
            c.execute(
                "UPDATE project_frames_local SET state_changed_at = 'old'",
                [],
            )
            .unwrap();
        };
        reset(&c);
        set_landed(&c, "p1", "u1", "/l/u1.fits", "1:1").unwrap();
        assert_ne!(stamp(&c).as_deref(), Some("old"), "wanted → held");
        reset(&c);
        set_landed(&c, "p1", "u1", "/l/u1.fits", "1:2").unwrap();
        assert_eq!(stamp(&c).as_deref(), Some("old"), "held → held is no move");
        set_local_state(&c, "p1", "u1", LocalState::Missing).unwrap();
        assert_ne!(stamp(&c).as_deref(), Some("old"), "held → missing");
        reset(&c);
        set_local_state(&c, "p1", "u1", LocalState::NotKept).unwrap();
        assert_ne!(stamp(&c).as_deref(), Some("old"), "missing → not_kept");
    }
}
