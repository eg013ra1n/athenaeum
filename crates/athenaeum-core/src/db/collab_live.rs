//! Local state of the collab v3 live exchange (wave 3, plan P5/P7/P8/P11).
//!
//! * `collab_live_meta` — hub-wide values: the device's `report_seq`
//!   counter (one per device, never goes back), the hub epoch, the account
//!   id seen in `hello`, the storage marker's store id, device and path
//!   (`collab::storage::marker`, T7 ruling: the path is recorded too, so a
//!   redesignation to a DIFFERENT folder can tell "still the same store" from
//!   "forget the old record and start fresh").
//! * `collab_my_claims` / `collab_outbox` — the device's claim set and the
//!   unsent changes to it. Written ONLY through [`record_claim_change`]
//!   (inside the same transaction as the local state change that causes it,
//!   collision C24) or [`add_implicit_claim`] (announce/version, no report).
//! * `collab_holder_devices` / `collab_holder_claims` — the persisted holder
//!   map of every project (spec §6.1).
//! * `collab_deletions` — settled deletions, for the L4 window and the 24 h
//!   second-deletion rule.
//! * `collab_quarantine` — the Changed files list (L5).

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

pub const META_REPORT_SEQ: &str = "report_seq";
pub const META_EPOCH: &str = "epoch";
pub const META_ACCOUNT_ID: &str = "account_id";
pub const META_STORE_ID: &str = "store_id";
pub const META_STORE_DEVICE: &str = "store_device";
pub const META_STORE_PATH: &str = "store_path";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOp {
    Add { content_version: i32 },
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRow {
    pub seq: i64,
    pub frame_uuid: String,
    pub op: ClaimOp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderDeviceRow {
    pub device: String,
    pub display_name: String,
    pub relay_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionRecord {
    pub project_id: String,
    pub frame_uuid: String,
    pub settled_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineRow {
    pub project_id: String,
    pub frame_uuid: String,
    pub path: String,
    pub detected_at: String,
    pub quarantined_version: i32,
    pub observed_size_mtime: Option<String>,
}

pub fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM collab_live_meta WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .optional()?)
}

/// A6 fix round 2: a project whose epoch-change re-announce was refused
/// because another device of this account was bound — the one case where a
/// later move of the binding to this device must run the re-announce check
/// again (besides a move from another device). Set, or cleared.
pub fn set_reannounce_refused(conn: &Connection, project_id: &str, refused: bool) -> Result<()> {
    let key = format!("reannounce_refused.{project_id}");
    if refused {
        meta_set(conn, &key, "1")
    } else {
        conn.execute("DELETE FROM collab_live_meta WHERE key = ?1", params![key])?;
        Ok(())
    }
}

/// Whether [`set_reannounce_refused`] marked the project.
pub fn reannounce_refused(conn: &Connection, project_id: &str) -> Result<bool> {
    Ok(meta_get(conn, &format!("reannounce_refused.{project_id}"))?.is_some())
}

/// Record that this device replaced `device_id` (a base64 public key) in
/// `account_id` (amendment A6 fix round 1). One upsert statement — a known
/// account is never overwritten by an unknown one.
pub fn record_replaced_device(
    conn: &Connection,
    device_id: &str,
    account_id: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO collab_replaced_devices (device_id, account_id) VALUES (?1, ?2)
         ON CONFLICT(device_id) DO UPDATE SET
            account_id = COALESCE(excluded.account_id, collab_replaced_devices.account_id)",
        params![device_id, account_id],
    )?;
    Ok(())
}

/// Take back a replaced-device record (A6 fix round 3: the hub refused the
/// retire it was written ahead of).
pub fn forget_replaced_device(conn: &Connection, device_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM collab_replaced_devices WHERE device_id = ?1",
        params![device_id],
    )?;
    Ok(())
}

/// Forget the live session's account id (`hello.accountId`) — at sign-out,
/// so a catalog signed into another account never answers with the
/// previous one (A6 fix round 3).
pub fn clear_account_id(conn: &Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM collab_live_meta WHERE key = ?1",
        params![META_ACCOUNT_ID],
    )?;
    Ok(())
}

/// Every device this device replaced: device id → the account it was
/// replaced in (`None` when unknown).
pub fn replaced_devices(
    conn: &Connection,
) -> Result<std::collections::HashMap<String, Option<String>>> {
    let mut stmt = conn.prepare("SELECT device_id, account_id FROM collab_replaced_devices")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()?;
    Ok(rows)
}

pub fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO collab_live_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

// ── the storage marker (wave 3 Task 7, `collab::storage::marker`) ──────────

/// The Collaboration root path this device last recorded a storage marker
/// for — `None` before the first successful designation. Compared against
/// the path being (re-)designated so a folder change forgets the stale
/// record instead of comparing a fresh folder's marker against one that
/// belongs to a different path entirely.
pub fn store_marker_path(conn: &Connection) -> Result<Option<String>> {
    meta_get(conn, META_STORE_PATH)
}

/// This device's recorded storage marker (store id + device), if any. `None`
/// when either half is missing — a partially-written record is never
/// treated as a match (the caller re-adopts from scratch).
pub fn recorded_store_marker(
    conn: &Connection,
) -> Result<Option<crate::collab::storage::marker::StoreMarker>> {
    let store_id = meta_get(conn, META_STORE_ID)?;
    let device_id = meta_get(conn, META_STORE_DEVICE)?;
    Ok(match (store_id, device_id) {
        (Some(store_id), Some(device_id)) => Some(crate::collab::storage::marker::StoreMarker {
            store_id,
            device_id,
        }),
        _ => None,
    })
}

/// Record a storage marker as this device's own, alongside the path it was
/// read/written at (T7 ruling: `collab_live_meta` records the path too).
pub fn record_store_marker(
    conn: &Connection,
    marker: &crate::collab::storage::marker::StoreMarker,
    path: &str,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    meta_set(&tx, META_STORE_ID, &marker.store_id)?;
    meta_set(&tx, META_STORE_DEVICE, &marker.device_id)?;
    meta_set(&tx, META_STORE_PATH, path)?;
    tx.commit()?;
    Ok(())
}

/// A designation this device refused because the folder's marker named
/// another device (`api::scan_roots::set_collaboration_dir`'s
/// `collab_other_device`/`collab_unknown_device` refusal): the path
/// attempted, the marker's device id, and whether that device is still an
/// ACTIVE device of this account (`Other`, offering a replace) or not
/// (`Unknown` — a foreign account's device, a plain-revoked device, or a
/// swapped disk; T7 fix round 2, ruling 1 — no automatic takeover, this is
/// classification for the UI only). So a later status read (Task 16) can
/// build the right prompt without re-touching the filesystem or the hub.
/// Cleared on the next successful designation.
pub const META_REFUSED_PATH: &str = "refused_path";
pub const META_REFUSED_DEVICE: &str = "refused_device";
pub const META_REFUSED_KIND: &str = "refused_kind";
/// `"1"` when the refusal was classified without the hub's answer (signed
/// out, offline, or the device list failed) — `Unknown` by design, and the
/// UI says so (Task 16).
pub const META_REFUSED_OFFLINE: &str = "refused_offline";
/// The replace offer an `Other` classification saw (Task 16 fix round 1):
/// the hub device id, its name and last-seen time, so the passive status
/// read can show the offer without asking the hub.
pub const META_REFUSED_OFFER_ID: &str = "refused_offer_id";
pub const META_REFUSED_OFFER_NAME: &str = "refused_offer_name";
pub const META_REFUSED_OFFER_LAST_SEEN: &str = "refused_offer_last_seen";
/// When the classification was last checked (RFC 3339, UTC) — what the UI
/// prints as "last checked …" (Task 17 concern 1). A re-check that finds the
/// same classification moves only this stamp.
pub const META_REFUSED_CHECKED_AT: &str = "refused_checked_at";

const REFUSAL_KEYS: [&str; 8] = [
    META_REFUSED_PATH,
    META_REFUSED_DEVICE,
    META_REFUSED_KIND,
    META_REFUSED_OFFLINE,
    META_REFUSED_OFFER_ID,
    META_REFUSED_OFFER_NAME,
    META_REFUSED_OFFER_LAST_SEEN,
    META_REFUSED_CHECKED_AT,
];

/// Whether a refused designation's marker names a device still active in
/// this account (offer a replace) or not (offer a take-over instead — spec
/// §9.1/§9.5, T7 fix round 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedDeviceKind {
    /// Listed among this account's active devices.
    Other,
    /// Not listed — a foreign account's device, a plain-revoked device, a
    /// swapped disk, or the hub could not be asked (signed out/offline).
    Unknown,
}

impl RefusedDeviceKind {
    fn as_db_str(self) -> &'static str {
        match self {
            RefusedDeviceKind::Other => "other",
            RefusedDeviceKind::Unknown => "unknown",
        }
    }

    /// An unrecognized stored value is never silently misread as `Other`
    /// (which carries a replace offer) — it defaults to the strictly safer
    /// `Unknown`.
    fn from_db_str(s: &str) -> Self {
        match s {
            "other" => RefusedDeviceKind::Other,
            _ => RefusedDeviceKind::Unknown,
        }
    }
}

/// The replace offer an `Other` classification saw, as recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedOffer {
    /// The hub's device id (what the replace takes).
    pub device_id: String,
    pub device_name: String,
    pub last_seen_at: Option<String>,
}

/// A recorded refused designation, with how its kind was decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedDesignation {
    pub path: String,
    /// The marker's device (its public key).
    pub device_id: String,
    pub kind: RefusedDeviceKind,
    /// Classified without the hub's device list (always `Unknown`).
    pub offline: bool,
    /// For `Other`: the offer the hub's device list gave.
    pub offer: Option<RecordedOffer>,
    /// When this classification was checked (RFC 3339, UTC); `None` for a
    /// record written before the stamp existed.
    pub checked_at: Option<String>,
}

impl RefusedDesignation {
    /// Everything but the check time — what "the same classification"
    /// compares.
    fn same_classification(&self, other: &RefusedDesignation) -> bool {
        self.path == other.path
            && self.device_id == other.device_id
            && self.kind == other.kind
            && self.offline == other.offline
            && self.offer == other.offer
    }
}

/// Record a folder's owner classification — THE writer of the refusal
/// record (Task 16 fix round 2): the designation refusal and the explicit
/// "Check again" both go through here. Returns `false` when the write was
/// SKIPPED because `new` is an offline answer (the hub could not be asked)
/// and the record already holds an online-verified classification of the
/// same folder and device — an unreachable hub never turns a verified
/// `Other` into an `Unknown` a take-over would accept (its check time stays
/// the verified one's). The same classification moves only the check time
/// (`true`). Otherwise every part is replaced.
pub fn record_classification(conn: &Connection, new: &RefusedDesignation) -> Result<bool> {
    // IMMEDIATE: it reads the kept record before it writes; a deferred
    // read-to-write upgrade under another writer fails at once with
    // SQLITE_BUSY, never waiting the busy timeout.
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let existing = refused_designation_detail(&tx)?;
    if let Some(e) = &existing {
        if new.offline && !e.offline && e.path == new.path && e.device_id == new.device_id {
            return Ok(false);
        }
        if e.same_classification(new) {
            if let Some(t) = new
                .checked_at
                .as_ref()
                .filter(|t| e.checked_at.as_ref() != Some(*t))
            {
                meta_set(&tx, META_REFUSED_CHECKED_AT, t)?;
                tx.commit()?;
            }
            return Ok(true);
        }
    }
    write_refusal(&tx, new)?;
    tx.commit()?;
    Ok(true)
}

/// Replace every part of the refusal record. Private: every writer goes
/// through the guarded [`record_classification`].
fn write_refusal(tx: &Connection, r: &RefusedDesignation) -> Result<()> {
    for key in REFUSAL_KEYS {
        tx.execute("DELETE FROM collab_live_meta WHERE key = ?1", params![key])?;
    }
    meta_set(tx, META_REFUSED_PATH, &r.path)?;
    meta_set(tx, META_REFUSED_DEVICE, &r.device_id)?;
    meta_set(tx, META_REFUSED_KIND, r.kind.as_db_str())?;
    meta_set(tx, META_REFUSED_OFFLINE, if r.offline { "1" } else { "0" })?;
    if let Some(o) = &r.offer {
        meta_set(tx, META_REFUSED_OFFER_ID, &o.device_id)?;
        meta_set(tx, META_REFUSED_OFFER_NAME, &o.device_name)?;
        if let Some(t) = &o.last_seen_at {
            meta_set(tx, META_REFUSED_OFFER_LAST_SEEN, t)?;
        }
    }
    if let Some(t) = &r.checked_at {
        meta_set(tx, META_REFUSED_CHECKED_AT, t)?;
    }
    Ok(())
}

/// The whole recorded refusal (a record written before the offline flag or
/// the offer existed reads as not offline, without an offer).
pub fn refused_designation_detail(conn: &Connection) -> Result<Option<RefusedDesignation>> {
    let Some((path, device_id, kind)) = refused_designation(conn)? else {
        return Ok(None);
    };
    let offline = meta_get(conn, META_REFUSED_OFFLINE)?.as_deref() == Some("1");
    let offer = match (
        meta_get(conn, META_REFUSED_OFFER_ID)?,
        meta_get(conn, META_REFUSED_OFFER_NAME)?,
    ) {
        (Some(device_id), Some(device_name)) => Some(RecordedOffer {
            device_id,
            device_name,
            last_seen_at: meta_get(conn, META_REFUSED_OFFER_LAST_SEEN)?,
        }),
        _ => None,
    };
    Ok(Some(RefusedDesignation {
        path,
        device_id,
        kind,
        offline,
        offer,
        checked_at: meta_get(conn, META_REFUSED_CHECKED_AT)?,
    }))
}

/// The last refused designation's `(path, device_id, kind)`, if any and if
/// every part is present.
pub fn refused_designation(
    conn: &Connection,
) -> Result<Option<(String, String, RefusedDeviceKind)>> {
    let path = meta_get(conn, META_REFUSED_PATH)?;
    let device_id = meta_get(conn, META_REFUSED_DEVICE)?;
    let kind = meta_get(conn, META_REFUSED_KIND)?;
    Ok(match (path, device_id, kind) {
        (Some(path), Some(device_id), Some(kind)) => {
            Some((path, device_id, RefusedDeviceKind::from_db_str(&kind)))
        }
        _ => None,
    })
}

pub fn clear_refused_designation(conn: &Connection) -> Result<()> {
    for key in REFUSAL_KEYS {
        conn.execute("DELETE FROM collab_live_meta WHERE key = ?1", params![key])?;
    }
    Ok(())
}

/// Forget the recorded storage marker (a Collaboration root cleared, or
/// about to be redesignated to a different path). The on-disk marker file
/// itself is never touched — an unmounted store is not deleted.
pub fn forget_store_marker(conn: &Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM collab_live_meta WHERE key IN (?1, ?2, ?3)",
        params![META_STORE_ID, META_STORE_DEVICE, META_STORE_PATH],
    )?;
    Ok(())
}

/// The highest journal sequence still in the outbox (any project), `0` when
/// the outbox is empty — the floor a lost or corrupt counter recovers to.
fn max_outbox_seq(conn: &Connection) -> Result<i64> {
    Ok(
        conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM collab_outbox", [], |r| {
            r.get(0)
        })?,
    )
}

/// The device's current journal counter, `0` when never set (and the outbox
/// is empty). A stored value that fails to parse is NEVER silently treated as
/// merely "unset" — it is logged (never swallowed, CLAUDE.md rule) and
/// recovered to the highest sequence still in the outbox (T1 ruling carried
/// by Task 6): every outbox row took its sequence from this counter, so the
/// counter is at least that high, and the caller's session start-up then
/// raises it further to `hello.reportSeq` ([`raise_report_seq`]) — never
/// lower than what a live hub already saw from us.
pub fn current_report_seq(conn: &Connection) -> Result<i64> {
    match meta_get(conn, META_REPORT_SEQ)? {
        None => max_outbox_seq(conn),
        Some(v) => match v.parse::<i64>() {
            Ok(n) => Ok(n),
            Err(error) => {
                let recovered = max_outbox_seq(conn)?;
                tracing::error!(value = %v, %error, report_seq = recovered, "report_seq value unparseable; recovering it from the outbox");
                Ok(recovered)
            }
        },
    }
}

/// The next journal sequence (hub rule: one monotonically increasing counter
/// per device, never reused, surviving restarts). ONE atomic
/// `UPSERT … RETURNING` (T1 ruling carried by Task 6): two pooled
/// connections can never both read `n` and both hand out `n + 1`. A corrupt
/// stored value recovers inside the same statement to the outbox's highest
/// sequence (see [`current_report_seq`]), and is logged first.
pub fn next_report_seq(conn: &Connection) -> Result<i64> {
    if let Some(v) = meta_get(conn, META_REPORT_SEQ)? {
        if let Err(error) = v.parse::<i64>() {
            tracing::error!(value = %v, %error, "report_seq value unparseable; recovering it from the outbox");
        }
    }
    Ok(conn.query_row(
        "INSERT INTO collab_live_meta (key, value)
         VALUES (?1, CAST((SELECT COALESCE(MAX(seq), 0) FROM collab_outbox) + 1 AS TEXT))
         ON CONFLICT(key) DO UPDATE SET value = CAST(
             (CASE WHEN value <> '' AND value NOT GLOB '*[^0-9]*'
                   THEN CAST(value AS INTEGER)
                   ELSE (SELECT COALESCE(MAX(seq), 0) FROM collab_outbox) END) + 1 AS TEXT)
         RETURNING CAST(value AS INTEGER)",
        [META_REPORT_SEQ],
        |r| r.get(0),
    )?)
}

/// Raise the counter to at least `at_least` (a `hello.reportSeq` above ours
/// means the local journal was lost). Never lowers it.
pub fn raise_report_seq(conn: &Connection, at_least: i64) -> Result<i64> {
    let cur = current_report_seq(conn)?;
    if at_least > cur {
        meta_set(conn, META_REPORT_SEQ, &at_least.to_string())?;
        return Ok(at_least);
    }
    Ok(cur)
}

/// Change the device's claim set and append the change to the outbox, under
/// one new journal sequence. Callers pass the SAME connection/transaction
/// that writes the state change (C24).
pub fn record_claim_change(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    op: ClaimOp,
) -> Result<i64> {
    let seq = next_report_seq(conn)?;
    match op {
        ClaimOp::Add { content_version } => {
            conn.execute(
                "INSERT INTO collab_my_claims (project_id, frame_uuid, content_version) VALUES (?1, ?2, ?3)
                 ON CONFLICT(project_id, frame_uuid) DO UPDATE SET content_version = excluded.content_version",
                params![project_id, frame_uuid, content_version],
            )?;
            conn.execute(
                "INSERT INTO collab_outbox (seq, project_id, frame_uuid, op, content_version) VALUES (?1, ?2, ?3, 'add', ?4)",
                params![seq, project_id, frame_uuid, content_version],
            )?;
        }
        ClaimOp::Remove => {
            conn.execute(
                "DELETE FROM collab_my_claims WHERE project_id = ?1 AND frame_uuid = ?2",
                params![project_id, frame_uuid],
            )?;
            conn.execute(
                "INSERT INTO collab_outbox (seq, project_id, frame_uuid, op, content_version) VALUES (?1, ?2, ?3, 'rm', 0)",
                params![seq, project_id, frame_uuid],
            )?;
        }
    }
    Ok(seq)
}

/// A claim the hub wrote itself (announce → `(uuid, 1)`, version → the new
/// content version). Enters the claim set and the digest; never the outbox.
pub fn add_implicit_claim(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    content_version: i32,
) -> Result<()> {
    conn.execute(
        "INSERT INTO collab_my_claims (project_id, frame_uuid, content_version) VALUES (?1, ?2, ?3)
         ON CONFLICT(project_id, frame_uuid) DO UPDATE SET content_version = excluded.content_version",
        params![project_id, frame_uuid, content_version],
    )?;
    Ok(())
}

/// Drop claims the hub refused (P9). No outbox row: the hub already
/// tombstoned them.
pub fn drop_claims(conn: &Connection, project_id: &str, frame_uuids: &[String]) -> Result<usize> {
    let mut n = 0;
    for u in frame_uuids {
        n += conn.execute(
            "DELETE FROM collab_my_claims WHERE project_id = ?1 AND frame_uuid = ?2",
            params![project_id, u],
        )?;
    }
    Ok(n)
}

/// The `last_error` a claim the hub refused leaves on its frame row (P9);
/// a manifest upsert of that frame reports the claim again
/// (`db::collab_frames::upsert_from_manifest`).
pub const REFUSED_CLAIM_ERROR: &str = "claim refused by the hub";

/// Drop ONE claim the hub refused (P9) — only while the claim set still
/// holds it at the refused `content_version` AND no outbox row above
/// `above_seq` (the refused report's sequence) names the frame. A frame
/// re-added while the report was in flight keeps its newer claim. No outbox
/// row: the hub already tombstoned it. Returns whether it was dropped.
pub fn drop_refused_claim(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    content_version: i32,
    above_seq: i64,
) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM collab_my_claims
         WHERE project_id = ?1 AND frame_uuid = ?2 AND content_version = ?3
           AND NOT EXISTS (SELECT 1 FROM collab_outbox
                           WHERE project_id = ?1 AND frame_uuid = ?2 AND seq > ?4)",
        params![project_id, frame_uuid, content_version, above_seq],
    )? > 0)
}

pub fn my_claims(conn: &Connection, project_id: &str) -> Result<Vec<(String, i32)>> {
    let mut stmt = conn.prepare(
        "SELECT frame_uuid, content_version FROM collab_my_claims WHERE project_id = ?1 ORDER BY frame_uuid",
    )?;
    let rows = stmt.query_map([project_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The frames `device`'s own row in the persisted holder map claims and
/// this device's claim set does not hold (final fix D-18): what the hub
/// still advertises for it that it no longer holds — an implicit claim a
/// `full: true` report keeps unless it lists the frame under `remove`
/// (hub 06198cc). Only frames with a known ordinal (a cached row) can be
/// named.
pub fn hub_claims_not_held(
    conn: &Connection,
    project_id: &str,
    device: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT f.frame_uuid FROM collab_holder_claims c
         JOIN project_frames_local f ON f.project_id = c.project_id AND f.frame_seq = c.frame_seq
         WHERE c.project_id = ?1 AND c.device = ?2
           AND NOT EXISTS (SELECT 1 FROM collab_my_claims m
                           WHERE m.project_id = c.project_id AND m.frame_uuid = f.frame_uuid)
         ORDER BY f.frame_uuid",
    )?;
    let rows = stmt.query_map(params![project_id, device], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn outbox(conn: &Connection, project_id: &str) -> Result<Vec<OutboxRow>> {
    let mut stmt = conn.prepare(
        "SELECT seq, frame_uuid, op, content_version FROM collab_outbox WHERE project_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map([project_id], |r| {
        let op: String = r.get(2)?;
        let cv: i32 = r.get(3)?;
        Ok(OutboxRow {
            seq: r.get(0)?,
            frame_uuid: r.get(1)?,
            op: if op == "add" {
                ClaimOp::Add {
                    content_version: cv,
                }
            } else {
                ClaimOp::Remove
            },
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn outbox_len(conn: &Connection, project_id: &str) -> Result<usize> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM collab_outbox WHERE project_id = ?1",
        [project_id],
        |r| r.get::<_, i64>(0),
    )? as usize)
}

pub fn outbox_projects(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT DISTINCT project_id FROM collab_outbox ORDER BY project_id")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Delete the outbox rows a successful report carried (`seq <= up_to_seq`).
/// Rows appended during the flush have higher sequences and stay.
pub fn ack_outbox(conn: &Connection, project_id: &str, up_to_seq: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM collab_outbox WHERE project_id = ?1 AND seq <= ?2",
        params![project_id, up_to_seq],
    )?)
}

/// A lost project (R14) or an epoch reload: forget claims, outbox and holders.
pub fn clear_project_live_state(conn: &Connection, project_id: &str) -> Result<()> {
    for table in [
        "collab_my_claims",
        "collab_outbox",
        "collab_holder_claims",
        "collab_holder_devices",
    ] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE project_id = ?1"),
            [project_id],
        )?;
    }
    Ok(())
}

/// Replace a project's holder map with a snapshot (one transaction is the
/// caller's).
pub fn replace_holders(
    conn: &Connection,
    project_id: &str,
    devices: &[HolderDeviceRow],
    claims: &[(String, i32, i32)],
) -> Result<()> {
    conn.execute(
        "DELETE FROM collab_holder_claims WHERE project_id = ?1",
        [project_id],
    )?;
    conn.execute(
        "DELETE FROM collab_holder_devices WHERE project_id = ?1",
        [project_id],
    )?;
    for d in devices {
        upsert_holder_device(conn, project_id, d)?;
    }
    let mut stmt = conn.prepare(
        "INSERT INTO collab_holder_claims (project_id, device, frame_seq, content_version) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (device, seq, cv) in claims {
        stmt.execute(params![project_id, device, seq, cv])?;
    }
    Ok(())
}

pub fn upsert_holder_device(
    conn: &Connection,
    project_id: &str,
    dev: &HolderDeviceRow,
) -> Result<()> {
    conn.execute(
        "INSERT INTO collab_holder_devices (project_id, device, display_name, relay_url) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(project_id, device) DO UPDATE SET display_name = excluded.display_name, relay_url = excluded.relay_url",
        params![project_id, dev.device, dev.display_name, dev.relay_url],
    )?;
    Ok(())
}

/// Apply one device's delta (`add` = `[frameSeq, contentVersion]`, `rm` =
/// `frameSeq`). An unknown device gets a placeholder row (its name arrives
/// with the next snapshot).
pub fn apply_holder_delta(
    conn: &Connection,
    project_id: &str,
    device: &str,
    add: &[(i32, i32)],
    rm: &[i32],
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO collab_holder_devices (project_id, device) VALUES (?1, ?2)",
        params![project_id, device],
    )?;
    for (seq, cv) in add {
        conn.execute(
            "INSERT INTO collab_holder_claims (project_id, device, frame_seq, content_version) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id, device, frame_seq) DO UPDATE SET content_version = excluded.content_version",
            params![project_id, device, seq, cv],
        )?;
    }
    for seq in rm {
        conn.execute(
            "DELETE FROM collab_holder_claims WHERE project_id = ?1 AND device = ?2 AND frame_seq = ?3",
            params![project_id, device, seq],
        )?;
    }
    Ok(())
}

pub fn load_holders(
    conn: &Connection,
    project_id: &str,
) -> Result<(Vec<HolderDeviceRow>, Vec<(String, i32, i32)>)> {
    let mut stmt = conn.prepare(
        "SELECT device, display_name, relay_url FROM collab_holder_devices WHERE project_id = ?1 ORDER BY device",
    )?;
    let devices = stmt
        .query_map([project_id], |r| {
            Ok(HolderDeviceRow {
                device: r.get(0)?,
                display_name: r.get(1)?,
                relay_url: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut stmt = conn.prepare(
        "SELECT device, frame_seq, content_version FROM collab_holder_claims WHERE project_id = ?1 ORDER BY device, frame_seq",
    )?;
    let claims = stmt
        .query_map([project_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((devices, claims))
}

pub fn record_deletion(
    conn: &Connection,
    project_id: &str,
    frame_uuid: &str,
    settled_at_ms: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO collab_deletions (project_id, frame_uuid, settled_at_ms) VALUES (?1, ?2, ?3)",
        params![project_id, frame_uuid, settled_at_ms],
    )?;
    Ok(())
}

pub fn deletions_since(conn: &Connection, since_ms: i64) -> Result<Vec<DeletionRecord>> {
    let mut stmt = conn.prepare(
        "SELECT project_id, frame_uuid, settled_at_ms FROM collab_deletions WHERE settled_at_ms >= ?1 ORDER BY settled_at_ms",
    )?;
    let rows = stmt.query_map([since_ms], |r| {
        Ok(DeletionRecord {
            project_id: r.get(0)?,
            frame_uuid: r.get(1)?,
            settled_at_ms: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn prune_deletions(conn: &Connection, older_than_ms: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM collab_deletions WHERE settled_at_ms < ?1",
        [older_than_ms],
    )?)
}

pub fn quarantine(conn: &Connection, row: &QuarantineRow) -> Result<()> {
    conn.execute(
        "INSERT INTO collab_quarantine (project_id, frame_uuid, path, quarantined_version, observed_size_mtime)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(project_id, frame_uuid) DO UPDATE SET path = excluded.path,
            observed_size_mtime = excluded.observed_size_mtime",
        params![
            row.project_id,
            row.frame_uuid,
            row.path,
            row.quarantined_version,
            row.observed_size_mtime
        ],
    )?;
    Ok(())
}

pub fn unquarantine(conn: &Connection, project_id: &str, frame_uuid: &str) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM collab_quarantine WHERE project_id = ?1 AND frame_uuid = ?2",
        params![project_id, frame_uuid],
    )?)
}

pub fn list_quarantine(conn: &Connection, project_id: &str) -> Result<Vec<QuarantineRow>> {
    let mut stmt = conn.prepare(
        "SELECT project_id, frame_uuid, path, detected_at, quarantined_version, observed_size_mtime
         FROM collab_quarantine WHERE project_id = ?1 ORDER BY detected_at, frame_uuid",
    )?;
    let rows = stmt.query_map([project_id], |r| {
        Ok(QuarantineRow {
            project_id: r.get(0)?,
            frame_uuid: r.get(1)?,
            path: r.get(2)?,
            detected_at: r.get(3)?,
            quarantined_version: r.get(4)?,
            observed_size_mtime: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn conn_with_project() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO collab_projects (project_id, slug, title, data_role, target_name,
               target_ra_deg, target_dec_deg, target_radius_deg, membership_version,
               snapshot_payload_b64, snapshot_signature_b64, members_json)
             VALUES ('p1','m31','M31','send_receive','M31',10.0,41.0,1.0,1,'','','[]')",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn report_seq_is_monotonic_and_can_be_raised() {
        let conn = conn_with_project();
        assert_eq!(current_report_seq(&conn).unwrap(), 0);
        assert_eq!(next_report_seq(&conn).unwrap(), 1);
        assert_eq!(next_report_seq(&conn).unwrap(), 2);
        assert_eq!(raise_report_seq(&conn, 120).unwrap(), 120);
        assert_eq!(next_report_seq(&conn).unwrap(), 121);
        // raising below the current value never lowers it
        assert_eq!(raise_report_seq(&conn, 5).unwrap(), 121);
    }

    #[test]
    fn a_corrupt_report_seq_value_is_never_silently_swallowed() {
        let conn = conn_with_project();
        for u in ["u1", "u2", "u3"] {
            record_claim_change(&conn, "p1", u, ClaimOp::Add { content_version: 1 }).unwrap();
        }
        meta_set(&conn, META_REPORT_SEQ, "not-a-number").unwrap();
        // Never panics, never propagates a parse error, never falls back to
        // 0 — it recovers to the highest sequence still in the outbox, so
        // the next sequence can never collide with a pending row.
        assert_eq!(current_report_seq(&conn).unwrap(), 3);
        assert_eq!(next_report_seq(&conn).unwrap(), 4);
        assert_eq!(next_report_seq(&conn).unwrap(), 5);
        // an empty outbox and a corrupt value → 1
        conn.execute("DELETE FROM collab_outbox", []).unwrap();
        meta_set(&conn, META_REPORT_SEQ, "12abc").unwrap();
        assert_eq!(next_report_seq(&conn).unwrap(), 1);
    }

    /// Two threads on two connections to one file database, started
    /// together: the single-statement counter never hands out a sequence
    /// twice (a read-then-write would, under this interleaving).
    #[test]
    fn next_report_seq_never_repeats_under_concurrent_connections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.db");
        {
            let c = Connection::open(&path).unwrap();
            init_db(&c).unwrap();
        }
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let (path, barrier) = (path.clone(), std::sync::Arc::clone(&barrier));
                std::thread::spawn(move || {
                    let c = Connection::open(&path).unwrap();
                    c.busy_timeout(std::time::Duration::from_secs(10)).unwrap();
                    barrier.wait();
                    (0..100)
                        .map(|_| next_report_seq(&c).unwrap())
                        .collect::<Vec<i64>>()
                })
            })
            .collect();
        let mut all: Vec<i64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        all.sort_unstable();
        let expected: Vec<i64> = (1..=200).collect();
        assert_eq!(all, expected, "every sequence exactly once");
    }

    #[test]
    fn claim_changes_update_the_claim_set_and_append_to_the_outbox() {
        let conn = conn_with_project();
        let s1 =
            record_claim_change(&conn, "p1", "u1", ClaimOp::Add { content_version: 1 }).unwrap();
        let s2 =
            record_claim_change(&conn, "p1", "u2", ClaimOp::Add { content_version: 2 }).unwrap();
        let s3 = record_claim_change(&conn, "p1", "u1", ClaimOp::Remove).unwrap();
        assert!(s1 < s2 && s2 < s3);
        assert_eq!(my_claims(&conn, "p1").unwrap(), vec![("u2".to_string(), 2)]);
        let rows = outbox(&conn, "p1").unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[2],
            OutboxRow {
                seq: s3,
                frame_uuid: "u1".into(),
                op: ClaimOp::Remove
            }
        );
        assert_eq!(ack_outbox(&conn, "p1", s2).unwrap(), 2);
        assert_eq!(outbox_len(&conn, "p1").unwrap(), 1);
        assert_eq!(outbox_projects(&conn).unwrap(), vec!["p1".to_string()]);
    }

    #[test]
    fn implicit_claims_never_touch_the_outbox() {
        let conn = conn_with_project();
        add_implicit_claim(&conn, "p1", "u9", 1).unwrap();
        assert_eq!(my_claims(&conn, "p1").unwrap(), vec![("u9".to_string(), 1)]);
        assert_eq!(outbox_len(&conn, "p1").unwrap(), 0);
        assert_eq!(drop_claims(&conn, "p1", &["u9".to_string()]).unwrap(), 1);
        assert!(my_claims(&conn, "p1").unwrap().is_empty());
    }

    #[test]
    fn holder_map_round_trips_and_deltas_apply() {
        let conn = conn_with_project();
        let dev = HolderDeviceRow {
            device: "AAA=".into(),
            display_name: "Anna".into(),
            relay_url: None,
        };
        replace_holders(
            &conn,
            "p1",
            &[dev.clone()],
            &[("AAA=".into(), 1, 1), ("AAA=".into(), 2, 1)],
        )
        .unwrap();
        apply_holder_delta(&conn, "p1", "AAA=", &[(3, 2)], &[1]).unwrap();
        let (devices, claims) = load_holders(&conn, "p1").unwrap();
        assert_eq!(devices, vec![dev]);
        assert_eq!(
            claims,
            vec![("AAA=".to_string(), 2, 1), ("AAA=".to_string(), 3, 2)]
        );
        // a delta for an unknown device creates a placeholder device row
        apply_holder_delta(&conn, "p1", "BBB=", &[(2, 1)], &[]).unwrap();
        assert_eq!(load_holders(&conn, "p1").unwrap().0.len(), 2);
    }

    #[test]
    fn clearing_a_project_drops_claims_outbox_and_holders() {
        let conn = conn_with_project();
        record_claim_change(&conn, "p1", "u1", ClaimOp::Add { content_version: 1 }).unwrap();
        apply_holder_delta(&conn, "p1", "AAA=", &[(1, 1)], &[]).unwrap();
        clear_project_live_state(&conn, "p1").unwrap();
        assert!(my_claims(&conn, "p1").unwrap().is_empty());
        assert_eq!(outbox_len(&conn, "p1").unwrap(), 0);
        assert!(load_holders(&conn, "p1").unwrap().1.is_empty());
    }

    #[test]
    fn deletions_and_quarantine_book_keeping() {
        let conn = conn_with_project();
        record_deletion(&conn, "p1", "u1", 1_000).unwrap();
        record_deletion(&conn, "p1", "u2", 400_000).unwrap();
        assert_eq!(deletions_since(&conn, 300_000).unwrap().len(), 1);
        assert_eq!(prune_deletions(&conn, 300_000).unwrap(), 1);
        let q = QuarantineRow {
            project_id: "p1".into(),
            frame_uuid: "u1".into(),
            path: "/c/m31/a/x.fits".into(),
            detected_at: String::new(),
            quarantined_version: 2,
            observed_size_mtime: Some("10:20".into()),
        };
        // the FK needs the frame row
        conn.execute(
            "INSERT INTO project_frames_local (project_id, frame_uuid, content_version, origin,
               publisher_account_id, publisher_display, file_name, filter_canonical, state,
               byte_size, xxh3, blake3) VALUES ('p1','u1',2,'replica','a','A','x.fits','R','published',10,'x','b')",
            [],
        )
        .unwrap();
        quarantine(&conn, &q).unwrap();
        let listed = list_quarantine(&conn, "p1").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].quarantined_version, 2);
        assert_eq!(unquarantine(&conn, "p1", "u1").unwrap(), 1);
    }

    #[test]
    fn storage_marker_round_trips_and_forgets() {
        let conn = conn_with_project();
        assert_eq!(recorded_store_marker(&conn).unwrap(), None);
        assert_eq!(store_marker_path(&conn).unwrap(), None);
        let marker = crate::collab::storage::marker::StoreMarker {
            store_id: "s1".into(),
            device_id: "ME".into(),
        };
        record_store_marker(&conn, &marker, "/collab/root").unwrap();
        assert_eq!(recorded_store_marker(&conn).unwrap(), Some(marker));
        assert_eq!(
            store_marker_path(&conn).unwrap().as_deref(),
            Some("/collab/root")
        );
        forget_store_marker(&conn).unwrap();
        assert_eq!(recorded_store_marker(&conn).unwrap(), None);
        assert_eq!(store_marker_path(&conn).unwrap(), None);
    }

    #[test]
    fn an_offline_answer_never_replaces_a_verified_classification() {
        let conn = conn_with_project();
        let verified = RefusedDesignation {
            path: "/collab/b".into(),
            device_id: "OLD-DEV".into(),
            kind: RefusedDeviceKind::Other,
            offline: false,
            offer: None,
            checked_at: None,
        };
        assert!(record_classification(&conn, &verified).unwrap());
        let offline = RefusedDesignation {
            kind: RefusedDeviceKind::Unknown,
            offline: true,
            ..verified.clone()
        };
        assert!(!record_classification(&conn, &offline).unwrap(), "skipped");
        assert_eq!(
            refused_designation_detail(&conn).unwrap(),
            Some(verified.clone())
        );
        // An online answer replaces it; so does an offline one for another
        // folder or device (the record has one slot).
        let online_unknown = RefusedDesignation {
            offline: false,
            ..offline.clone()
        };
        assert!(record_classification(&conn, &online_unknown).unwrap());
        assert_eq!(
            refused_designation_detail(&conn).unwrap(),
            Some(online_unknown)
        );
        let elsewhere = RefusedDesignation {
            path: "/collab/c".into(),
            ..offline.clone()
        };
        assert!(record_classification(&conn, &elsewhere).unwrap());
        assert_eq!(
            refused_designation_detail(&conn).unwrap(),
            Some(elsewhere.clone())
        );
        // An offline answer over an offline record for the same folder: written.
        assert!(record_classification(&conn, &elsewhere).unwrap());
    }

    /// A read-then-write transaction must wait for another writer, not fail:
    /// a deferred transaction's read-to-write upgrade under another writer's
    /// lock answers `SQLITE_BUSY` at once (the busy timeout never applies),
    /// so the classification was lost. Another connection holds the write
    /// lock for 300 ms (as a landing or the storage task's scope pass does);
    /// the record waits for it and is written.
    #[test]
    fn a_classification_waits_for_another_writer_instead_of_failing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("catalog.db");
        let conn = Connection::open(&path).unwrap();
        crate::db::SqliteConnectionManager::setup_connection(&conn).unwrap();
        init_db(&conn).unwrap();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let other = Connection::open(&path).unwrap();
            crate::db::SqliteConnectionManager::setup_connection(&other).unwrap();
            other.execute_batch("BEGIN IMMEDIATE").unwrap();
            held_tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            other.execute_batch("COMMIT").unwrap();
        });
        held_rx.recv().unwrap();
        let refusal = RefusedDesignation {
            path: "/collab/b".into(),
            device_id: "OLD-DEV".into(),
            kind: RefusedDeviceKind::Other,
            offline: false,
            offer: None,
            checked_at: None,
        };
        let recorded = record_classification(&conn, &refusal);
        writer.join().unwrap();
        assert!(
            recorded.expect("the record waits for the other writer"),
            "written"
        );
        assert_eq!(refused_designation_detail(&conn).unwrap(), Some(refusal));
    }

    /// Task 17 concern 1: the record keeps when it was checked; a re-check
    /// with the same classification moves only that stamp, and an offline
    /// answer over a verified record keeps the verified stamp.
    #[test]
    fn a_recheck_moves_only_the_check_time() {
        let conn = conn_with_project();
        let first = RefusedDesignation {
            path: "/collab/b".into(),
            device_id: "OLD-DEV".into(),
            kind: RefusedDeviceKind::Other,
            offline: false,
            offer: Some(RecordedOffer {
                device_id: "hub-id".into(),
                device_name: "Old laptop".into(),
                last_seen_at: None,
            }),
            checked_at: Some("2026-09-27T10:00:00+00:00".into()),
        };
        assert!(record_classification(&conn, &first).unwrap());
        assert_eq!(
            refused_designation_detail(&conn).unwrap(),
            Some(first.clone())
        );
        let again = RefusedDesignation {
            checked_at: Some("2026-09-27T11:00:00+00:00".into()),
            ..first.clone()
        };
        assert!(record_classification(&conn, &again).unwrap());
        assert_eq!(
            refused_designation_detail(&conn).unwrap(),
            Some(again.clone())
        );
        let offline = RefusedDesignation {
            kind: RefusedDeviceKind::Unknown,
            offline: true,
            offer: None,
            checked_at: Some("2026-09-27T12:00:00+00:00".into()),
            ..first
        };
        assert!(!record_classification(&conn, &offline).unwrap());
        assert_eq!(
            refused_designation_detail(&conn).unwrap(),
            Some(again),
            "the verified record and its check time stay"
        );
    }

    #[test]
    fn refused_designation_round_trips_and_clears() {
        let conn = conn_with_project();
        assert_eq!(refused_designation(&conn).unwrap(), None);
        let other = RefusedDesignation {
            path: "/collab/root".into(),
            device_id: "OTHER-DEVICE".into(),
            kind: RefusedDeviceKind::Other,
            offline: false,
            offer: Some(RecordedOffer {
                device_id: "hub-id".into(),
                device_name: "Old laptop".into(),
                last_seen_at: Some("2026-09-01T00:00:00Z".into()),
            }),
            checked_at: None,
        };
        assert!(record_classification(&conn, &other).unwrap());
        assert_eq!(
            refused_designation(&conn).unwrap(),
            Some((
                "/collab/root".to_string(),
                "OTHER-DEVICE".to_string(),
                RefusedDeviceKind::Other
            ))
        );
        assert_eq!(refused_designation_detail(&conn).unwrap(), Some(other));
        // A later record replaces every part — no stale offer survives.
        let unknown = RefusedDesignation {
            path: "/collab/root2".into(),
            device_id: "GHOST".into(),
            kind: RefusedDeviceKind::Unknown,
            offline: true,
            offer: None,
            checked_at: None,
        };
        assert!(record_classification(&conn, &unknown).unwrap());
        assert_eq!(refused_designation_detail(&conn).unwrap(), Some(unknown));
        clear_refused_designation(&conn).unwrap();
        assert_eq!(refused_designation(&conn).unwrap(), None);
        assert_eq!(refused_designation_detail(&conn).unwrap(), None);
        for key in REFUSAL_KEYS {
            assert_eq!(meta_get(&conn, key).unwrap(), None, "{key}");
        }
    }
}
