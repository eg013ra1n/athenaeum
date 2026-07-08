//! Durable store for the sync engine.
//!
//! [`SyncStore`] is a **synchronous** (rusqlite) trait called from the engine's
//! worker task. Its SQL — DDL and every statement — lives here so both the
//! standalone implementation ([`StandaloneSyncStore`], this task) and the
//! catalog-backed one (`CatalogSyncStore`, task A7) share one schema. That later
//! implementation reuses [`DDL_OUTBOUND`] / [`DDL_HISTORY`] / [`DDL_INDEXES`]
//! verbatim from a `db/schema.rs` migration; A4 does not touch the app catalog.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::sharing::types::{FrameReceipt, NodeId, PackageId, ReceiptOutcome};

use super::models::{Direction, HistoryQuery, HistoryRow, OutboundRow, OutboundState};
use super::{node_id_from_hex, node_id_hex, now_iso};

/// `sync_outbound` — the durable outbound state machine, one row per package.
pub const DDL_OUTBOUND: &str = "CREATE TABLE IF NOT EXISTS sync_outbound (
    id INTEGER PRIMARY KEY,
    package_ref TEXT NOT NULL,
    peer TEXT NOT NULL,
    state TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    confirmed_at TEXT
)";

/// `sync_history` — append-only per-frame transfer audit log.
pub const DDL_HISTORY: &str = "CREATE TABLE IF NOT EXISTS sync_history (
    id INTEGER PRIMARY KEY,
    frame_uuid TEXT,
    filename TEXT,
    object TEXT,
    peer_device TEXT,
    direction TEXT,
    bytes INTEGER,
    started_at TEXT,
    finished_at TEXT,
    outcome TEXT
)";

/// `sync_receipts` — the receiver's durable per-frame verdict log (task A7).
///
/// One row per `(package_id, frame_uuid)`: the receipt the receiver returned to
/// the sender for that frame. It is the source of truth for **ack replay** — a
/// re-received announce for a package that is already fully receipted is re-acked
/// straight from this log without re-fetching or re-ingesting. Receiver-only:
/// the standalone (sender) store never writes it, but the DDL lives here so
/// `db/schema.rs` and [`CatalogSyncStore`] share one definition.
pub const DDL_RECEIPTS: &str = "CREATE TABLE IF NOT EXISTS sync_receipts (
    package_id TEXT NOT NULL,
    frame_uuid TEXT NOT NULL,
    xxh3 TEXT NOT NULL,
    outcome TEXT NOT NULL,
    received_at TEXT NOT NULL,
    PRIMARY KEY (package_id, frame_uuid)
)";

/// `sync_sources` — the app sender's package → source-file linkage (task M4).
///
/// The app-side equivalent of Perseus's `perseus_seen`: it maps a
/// `sync_outbound.package_ref` back to the ORIGINAL catalog source file(s) the
/// package was built from, so retention can resolve a *confirmed package* to the
/// disk files it may reclaim. Unlike Perseus (one file per package), an app
/// selection/scan-batch package carries MANY files, so this is a one-package →
/// many-rows table keyed on `(package_ref, path)`.
///
/// Columns mirror `perseus_seen`'s retention semantics:
/// - `file_id` — the catalog `files.id`, so retention can do a *catalog-consistent*
///   delete (remove the `files` row → CASCADE to `frames`) alongside the disk file.
///   Nullable only defensively (every app package is built from catalog frames).
/// - `size` / `mtime` — the stat recorded at package-build time; retention's
///   last-line TOCTOU guard re-stats the file and requires an exact match before
///   deleting, so a since-rewritten path is never destroyed.
/// - `deleted_at` — stamped once retention has handled the linkage, so it never
///   surfaces again (the row is kept as a durable audit trail).
///
/// Receiver-agnostic and app-only: Perseus keeps its own `perseus_seen`. The DDL
/// lives here beside the other sync tables so `db/schema.rs` materialises it in
/// the app catalog through the one shared definition.
pub const DDL_SYNC_SOURCES: &str = "CREATE TABLE IF NOT EXISTS sync_sources (
    package_ref TEXT NOT NULL,
    file_id INTEGER,
    path TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    enqueued_at TEXT NOT NULL,
    deleted_at TEXT,
    PRIMARY KEY (package_ref, path)
)";

/// Search indexes for [`SyncStore::search_history`].
pub const DDL_INDEXES: [&str; 3] = [
    "CREATE INDEX IF NOT EXISTS idx_sync_history_filename ON sync_history(filename)",
    "CREATE INDEX IF NOT EXISTS idx_sync_history_object ON sync_history(object)",
    "CREATE INDEX IF NOT EXISTS idx_sync_history_started_at ON sync_history(started_at)",
];

/// Durable persistence for the outbound state machine + transfer history.
///
/// Synchronous by contract — the engine worker calls these directly. Every
/// implementation is `Send + Sync` so it can live behind an `Arc<dyn SyncStore>`
/// shared by the worker and the [`SyncEngineHandle`](super::engine::SyncEngineHandle).
pub trait SyncStore: Send + Sync {
    /// Insert a new package in [`Queued`](OutboundState::Queued); returns its id.
    fn enqueue(&self, package_ref: &str, peer: NodeId) -> Result<i64>;

    /// Force one outbound row to a new state.
    fn set_state(&self, id: i64, s: OutboundState) -> Result<()>;

    /// Increment `attempts` and return the new value (drives max-attempts).
    fn bump_attempts(&self, id: i64) -> Result<u32>;

    /// Every non-terminal outbound row — the crash-resume enumeration.
    fn non_terminal(&self) -> Result<Vec<OutboundRow>>;

    /// Every package currently in [`Confirmed`](OutboundState::Confirmed),
    /// oldest-confirmed-first (`confirmed_at ASC, id ASC`).
    ///
    /// This is the retention evaluator's **sole** source of deletable
    /// candidates (task A8) — the single chokepoint that enforces the hard
    /// invariant that only *confirmed* (fully received) data is ever eligible
    /// for deletion. A package in any non-confirmed state (`queued`,
    /// `announced`, `transferring`, `failed`, …) is NEVER returned here and so
    /// can never reach a deleter, regardless of policy or disk pressure. The
    /// oldest-first ordering is what `DiskPct` retention relies on to delete
    /// the oldest confirmed data first.
    fn confirmed(&self) -> Result<Vec<OutboundRow>>;

    /// Mark a package [`Confirmed`](OutboundState::Confirmed) (idempotent: a
    /// no-op if already confirmed). `receipts` are the peer's per-frame verdicts;
    /// A4 records them as [`HistoryRow`]s via [`append_history`](Self::append_history)
    /// rather than a separate table.
    fn confirm(&self, id: i64, receipts: &[FrameReceipt]) -> Result<()>;

    /// Append one audit row.
    fn append_history(&self, h: HistoryRow) -> Result<()>;

    /// Exact-match, newest-first history search (see [`HistoryQuery`]).
    fn search_history(&self, q: HistoryQuery) -> Result<Vec<HistoryRow>>;
}

/// Raw column tuple for a `sync_outbound` row, parsed into [`OutboundRow`] by
/// [`to_outbound`] so fallible text parsing happens outside the rusqlite closure.
type OutboundRaw = (i64, String, String, String, i64, String, Option<String>);

fn to_outbound(raw: OutboundRaw) -> Result<OutboundRow> {
    let (id, package_ref, peer_hex, state, attempts, created_at, confirmed_at) = raw;
    Ok(OutboundRow {
        id,
        package_ref,
        peer: node_id_from_hex(&peer_hex)?,
        state: OutboundState::from_db(&state)?,
        attempts: attempts.max(0) as u32,
        created_at,
        confirmed_at,
    })
}

/// Raw column tuple for a `sync_history` row.
type HistoryRaw = (
    String,
    String,
    Option<String>,
    String,
    String,
    i64,
    String,
    Option<String>,
    String,
);

fn to_history(raw: HistoryRaw) -> Result<HistoryRow> {
    let (frame_uuid, filename, object, peer_device, direction, bytes, started_at, finished_at, outcome) =
        raw;
    Ok(HistoryRow {
        frame_uuid,
        filename,
        object,
        peer_device,
        direction: Direction::from_db(&direction)?,
        bytes: bytes.max(0) as u64,
        started_at,
        finished_at,
        outcome,
    })
}

const OUTBOUND_COLS: &str =
    "id, package_ref, peer, state, attempts, created_at, confirmed_at";
const HISTORY_COLS: &str =
    "frame_uuid, filename, object, peer_device, direction, bytes, started_at, finished_at, outcome";

// ── Free functions on a raw connection ──────────────────────────────────────
//
// The history / receipt SQL is defined once here as plain functions over a
// `&Connection` so both store implementations AND the receiver's per-frame
// ingest transaction (task A7 — which must land file+frame+header+receipt+history
// atomically on ONE connection) share a single source. `StandaloneSyncStore`
// and `CatalogSyncStore` delegate to these; `sync::ingest` calls them directly
// inside its transaction.

/// Append one audit row via `conn` (participates in the caller's transaction).
pub fn insert_history_row(conn: &Connection, h: &HistoryRow) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_history
         (frame_uuid, filename, object, peer_device, direction, bytes, started_at, finished_at, outcome)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            h.frame_uuid,
            h.filename,
            h.object,
            h.peer_device,
            h.direction.as_str(),
            h.bytes as i64,
            h.started_at,
            h.finished_at,
            h.outcome,
        ],
    )
    .context("insert sync_history")?;
    Ok(())
}

/// Exact-match, newest-first history search (see [`HistoryQuery`]).
pub fn search_history_rows(conn: &Connection, q: &HistoryQuery) -> Result<Vec<HistoryRow>> {
    let mut sql = format!("SELECT {HISTORY_COLS} FROM sync_history WHERE 1 = 1");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(filename) = &q.filename {
        sql.push_str(" AND filename = ?");
        args.push(Box::new(filename.clone()));
    }
    if let Some(object) = &q.object {
        sql.push_str(" AND object = ?");
        args.push(Box::new(object.clone()));
    }
    if let Some(direction) = &q.direction {
        sql.push_str(" AND direction = ?");
        args.push(Box::new(direction.as_str().to_string()));
    }
    if let Some(peer) = &q.peer {
        sql.push_str(" AND peer_device = ?");
        args.push(Box::new(peer.clone()));
    }
    sql.push_str(" ORDER BY started_at DESC, id DESC LIMIT ?");
    args.push(Box::new(q.limit as i64));

    let params_ref: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).context("prepare search_history")?;
    let raws = stmt
        .query_map(params_ref.as_slice(), |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        })
        .context("query search_history")?
        .collect::<rusqlite::Result<Vec<HistoryRaw>>>()
        .context("collect search_history")?;
    raws.into_iter().map(to_history).collect()
}

/// Load every `sync_outbound` row in the terminal `confirmed` state,
/// oldest-confirmed-first (`confirmed_at ASC, id ASC`). Defined once here so
/// both store implementations share the retention-candidate query verbatim; the
/// SQL filters on `state = 'confirmed'` so no non-confirmed row is ever
/// returned. See [`SyncStore::confirmed`].
pub fn confirmed_outbound_rows(conn: &Connection) -> Result<Vec<OutboundRow>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {OUTBOUND_COLS} FROM sync_outbound
             WHERE state = 'confirmed'
             ORDER BY confirmed_at ASC, id ASC"
        ))
        .context("prepare confirmed_outbound_rows")?;
    let raws = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .context("query confirmed_outbound_rows")?
        .collect::<rusqlite::Result<Vec<OutboundRaw>>>()
        .context("collect confirmed_outbound_rows")?;
    raws.into_iter().map(to_outbound).collect()
}

/// Every `sync_outbound` row, newest-first, capped at `limit`. Unlike
/// [`confirmed_outbound_rows`] there is no state filter — this is the Perseus web
/// status page's "sent" list, which shows queued/announced/transferring/confirmed/
/// failed packages alike. `ORDER BY id DESC` puts the most recently enqueued
/// packages first; `LIMIT` bounds the response for a long-running node whose
/// confirmed rows accumulate (retention deletes source files, never these rows).
pub fn all_outbound_rows(conn: &Connection, limit: u32) -> Result<Vec<OutboundRow>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {OUTBOUND_COLS} FROM sync_outbound
             ORDER BY id DESC LIMIT ?1"
        ))
        .context("prepare all_outbound_rows")?;
    let raws = stmt
        .query_map(params![limit], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .context("query all_outbound_rows")?
        .collect::<rusqlite::Result<Vec<OutboundRaw>>>()
        .context("collect all_outbound_rows")?;
    raws.into_iter().map(to_outbound).collect()
}

/// One `sync_sources` row: a live (or historic) linkage from an app package back
/// to one catalog source file it was built from. See [`DDL_SYNC_SOURCES`].
///
/// `size`/`mtime_ms` are the stat recorded when the package was built — the
/// retention deleter's last-line TOCTOU guard re-stats the file and requires an
/// exact match before removal, so a since-rewritten path is never destroyed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSourceRow {
    pub package_ref: String,
    pub file_id: Option<i64>,
    pub path: String,
    pub size: u64,
    pub mtime_ms: i64,
}

/// Record (insert or overwrite) one source-file linkage for `package_ref` at
/// build time. Idempotent by `(package_ref, path)`: a rebuild of the same
/// selection overwrites the recorded stat and clears any prior `deleted_at` (the
/// file is a live source again). See [`SyncSourceRow`].
pub fn insert_sync_source(
    conn: &Connection,
    package_ref: &str,
    file_id: Option<i64>,
    path: &str,
    size: u64,
    mtime_ms: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_sources (package_ref, file_id, path, size, mtime, enqueued_at, deleted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)
         ON CONFLICT(package_ref, path) DO UPDATE SET
             file_id = excluded.file_id,
             size = excluded.size,
             mtime = excluded.mtime,
             enqueued_at = excluded.enqueued_at,
             deleted_at = NULL",
        params![package_ref, file_id, path, size as i64, mtime_ms, now_iso()],
    )
    .context("insert sync_sources")?;
    Ok(())
}

/// Every *live* (`deleted_at IS NULL`) source linkage for `package_ref`, ordered
/// by path for a deterministic pass. Retention resolves a confirmed package to
/// exactly the source files it may reclaim through this call.
pub fn live_sources_for_package(conn: &Connection, package_ref: &str) -> Result<Vec<SyncSourceRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT package_ref, file_id, path, size, mtime FROM sync_sources
             WHERE package_ref = ?1 AND deleted_at IS NULL
             ORDER BY path ASC",
        )
        .context("prepare live_sources_for_package")?;
    let rows = stmt
        .query_map(params![package_ref], |r| {
            Ok(SyncSourceRow {
                package_ref: r.get(0)?,
                file_id: r.get(1)?,
                path: r.get(2)?,
                size: {
                    let s: i64 = r.get(3)?;
                    s.max(0) as u64
                },
                mtime_ms: r.get(4)?,
            })
        })
        .context("query live_sources_for_package")?
        .collect::<rusqlite::Result<Vec<SyncSourceRow>>>()
        .context("collect live_sources_for_package")?;
    Ok(rows)
}

/// Stamp a source linkage `deleted_at = now` after retention has handled it, so
/// it never surfaces via [`live_sources_for_package`] again (the row survives as
/// a durable audit trail).
pub fn mark_sync_source_deleted(conn: &Connection, package_ref: &str, path: &str) -> Result<()> {
    conn.execute(
        "UPDATE sync_sources SET deleted_at = ?3 WHERE package_ref = ?1 AND path = ?2",
        params![package_ref, path, now_iso()],
    )
    .context("mark sync_sources row deleted")?;
    Ok(())
}

/// Stable text encoding of a [`ReceiptOutcome`] for the `sync_receipts.outcome`
/// column: `ingested` / `duplicate` / `rejected:<reason>`.
pub fn receipt_outcome_to_db(o: &ReceiptOutcome) -> String {
    match o {
        ReceiptOutcome::Ingested => "ingested".to_string(),
        ReceiptOutcome::Duplicate => "duplicate".to_string(),
        ReceiptOutcome::Rejected(msg) => format!("rejected:{msg}"),
    }
}

/// Parse the `sync_receipts.outcome` text back into a [`ReceiptOutcome`].
/// Unknown/legacy values decode as `Rejected` carrying the raw text so a receipt
/// is never silently dropped.
pub fn receipt_outcome_from_db(s: &str) -> ReceiptOutcome {
    match s {
        "ingested" => ReceiptOutcome::Ingested,
        "duplicate" => ReceiptOutcome::Duplicate,
        other => ReceiptOutcome::Rejected(other.strip_prefix("rejected:").unwrap_or(other).to_string()),
    }
}

/// Upsert one receipt row via `conn` (participates in the caller's transaction).
/// Idempotent by `(package_id, frame_uuid)` — a re-ingest of the same package
/// overwrites the prior verdict rather than erroring on the primary key.
pub fn insert_receipt(
    conn: &Connection,
    package_id: &str,
    r: &FrameReceipt,
    received_at: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_receipts (package_id, frame_uuid, xxh3, outcome, received_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(package_id, frame_uuid) DO UPDATE SET
             xxh3 = excluded.xxh3, outcome = excluded.outcome, received_at = excluded.received_at",
        params![package_id, r.frame_uuid, r.xxh3, receipt_outcome_to_db(&r.outcome), received_at],
    )
    .context("insert sync_receipts")?;
    Ok(())
}

/// Count every receipt row recorded for `package_id`, regardless of outcome.
/// NOT the ack-replay guard (a `Rejected` receipt must never count as
/// "satisfied") — see [`count_satisfied_receipts`] for that. Kept as a plain
/// total for callers that want raw coverage (e.g. "has this package been
/// touched at all").
pub fn count_receipts(conn: &Connection, package_id: &str) -> Result<u32> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_receipts WHERE package_id = ?1",
            params![package_id],
            |r| r.get(0),
        )
        .context("count sync_receipts")?;
    Ok(n.max(0) as u32)
}

/// Count the receipt rows recorded for `package_id` whose outcome is NOT a
/// rejection (the ack-replay guard: a package whose *satisfied* receipt count
/// equals its announced `frame_count` is fully receipted — every frame
/// ingested or duplicate — and can be re-acked from the log).
///
/// A `Rejected` receipt must never count toward this total: a package with any
/// rejected frame still needs a redelivery attempt for that frame, so treating
/// it as "fully receipted" would permanently strand the rejected frame behind
/// the replay guard, re-acking the same stale `Rejected` verdict forever
/// instead of ever re-fetching and re-ingesting it.
pub fn count_satisfied_receipts(conn: &Connection, package_id: &str) -> Result<u32> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_receipts WHERE package_id = ?1 AND outcome NOT LIKE 'rejected:%'",
            params![package_id],
            |r| r.get(0),
        )
        .context("count satisfied sync_receipts")?;
    Ok(n.max(0) as u32)
}

/// Load every receipt recorded for `package_id`, reconstructed as
/// [`FrameReceipt`]s ready to replay in an `ack`.
pub fn load_receipts(conn: &Connection, package_id: &str) -> Result<Vec<FrameReceipt>> {
    let mut stmt = conn
        .prepare("SELECT frame_uuid, xxh3, outcome FROM sync_receipts WHERE package_id = ?1 ORDER BY frame_uuid")
        .context("prepare load_receipts")?;
    let rows = stmt
        .query_map(params![package_id], |r| {
            let frame_uuid: String = r.get(0)?;
            let xxh3: String = r.get(1)?;
            let outcome: String = r.get(2)?;
            Ok(FrameReceipt {
                frame_uuid,
                xxh3,
                outcome: receipt_outcome_from_db(&outcome),
            })
        })
        .context("query load_receipts")?
        .collect::<rusqlite::Result<Vec<FrameReceipt>>>()
        .context("collect load_receipts")?;
    Ok(rows)
}

/// Standalone [`SyncStore`] backed by its own WAL SQLite file. Used by the
/// Perseus agent and by the engine's tests. The connection is guarded by a
/// [`Mutex`] so the store is `Sync`; no lock is ever held across an `.await`
/// (all methods here are synchronous).
pub struct StandaloneSyncStore {
    conn: Mutex<Connection>,
}

impl StandaloneSyncStore {
    /// Open (creating if absent) a standalone sync DB at `path`, apply the WAL
    /// pragmas, and create the tables/indexes idempotently.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let conn = Connection::open(path)
            .with_context(|| format!("open sync db {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .context("configure sync db pragmas")?;
        conn.execute(DDL_OUTBOUND, [])
            .context("create sync_outbound")?;
        conn.execute(DDL_HISTORY, []).context("create sync_history")?;
        for idx in DDL_INDEXES {
            conn.execute(idx, []).context("create sync_history index")?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Fetch a single outbound row by id. Inherent helper (not part of the
    /// trait) — used by tests and callers that need a terminal-state read.
    pub fn get_outbound(&self, id: i64) -> Result<Option<OutboundRow>> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        let raw = conn
            .query_row(
                &format!("SELECT {OUTBOUND_COLS} FROM sync_outbound WHERE id = ?1"),
                params![id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .optional()
            .context("query sync_outbound by id")?;
        raw.map(to_outbound).transpose()
    }

    /// Every outbound row, newest-first, capped at `limit` — the Perseus web
    /// status page's "sent" list. Delegates to [`all_outbound_rows`]. Inherent
    /// (not part of [`SyncStore`]) because only the web layer needs an unfiltered
    /// listing; the engine drives itself off the state-filtered trait methods.
    pub fn all_outbound(&self, limit: u32) -> Result<Vec<OutboundRow>> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        all_outbound_rows(&conn, limit)
    }
}

impl SyncStore for StandaloneSyncStore {
    fn enqueue(&self, package_ref: &str, peer: NodeId) -> Result<i64> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        conn.execute(
            "INSERT INTO sync_outbound (package_ref, peer, state, attempts, created_at)
             VALUES (?1, ?2, ?3, 0, ?4)",
            params![
                package_ref,
                node_id_hex(&peer),
                OutboundState::Queued.as_str(),
                now_iso()
            ],
        )
        .context("insert sync_outbound")?;
        Ok(conn.last_insert_rowid())
    }

    fn set_state(&self, id: i64, s: OutboundState) -> Result<()> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        conn.execute(
            "UPDATE sync_outbound SET state = ?1 WHERE id = ?2",
            params![s.as_str(), id],
        )
        .with_context(|| format!("set state {} for outbound {id}", s.as_str()))?;
        Ok(())
    }

    fn bump_attempts(&self, id: i64) -> Result<u32> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        conn.execute(
            "UPDATE sync_outbound SET attempts = attempts + 1 WHERE id = ?1",
            params![id],
        )
        .with_context(|| format!("bump attempts for outbound {id}"))?;
        let attempts: i64 = conn
            .query_row(
                "SELECT attempts FROM sync_outbound WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .with_context(|| format!("read attempts for outbound {id}"))?;
        Ok(attempts.max(0) as u32)
    }

    fn non_terminal(&self) -> Result<Vec<OutboundRow>> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {OUTBOUND_COLS} FROM sync_outbound
                 WHERE state NOT IN ('confirmed', 'failed')
                 ORDER BY id ASC"
            ))
            .context("prepare non_terminal")?;
        let raws = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })
            .context("query non_terminal")?
            .collect::<rusqlite::Result<Vec<OutboundRaw>>>()
            .context("collect non_terminal")?;
        raws.into_iter().map(to_outbound).collect()
    }

    fn confirmed(&self) -> Result<Vec<OutboundRow>> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        confirmed_outbound_rows(&conn)
    }

    fn confirm(&self, id: i64, receipts: &[FrameReceipt]) -> Result<()> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        // Idempotent: only a non-confirmed row transitions (guards a duplicate
        // ack that slipped past the engine's in-flight map).
        let changed = conn
            .execute(
                "UPDATE sync_outbound SET state = ?1, confirmed_at = ?2
                 WHERE id = ?3 AND state <> ?1",
                params![OutboundState::Confirmed.as_str(), now_iso(), id],
            )
            .with_context(|| format!("confirm outbound {id}"))?;
        tracing::debug!(
            package_id = id,
            receipts = receipts.len(),
            changed,
            "sync store confirm"
        );
        Ok(())
    }

    fn append_history(&self, h: HistoryRow) -> Result<()> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        insert_history_row(&conn, &h)
    }

    fn search_history(&self, q: HistoryQuery) -> Result<Vec<HistoryRow>> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        search_history_rows(&conn, &q)
    }
}

/// [`SyncStore`] backed by the **app catalog** SQLite file (task A7).
///
/// Same trait, same DDL as [`StandaloneSyncStore`] — the difference is only
/// *which* database file the sync tables live in. The app catalog already runs
/// [`init_db`](crate::db::schema::init_db) (which now creates the sync tables),
/// so `open` just attaches a WAL connection and idempotently ensures the tables
/// exist; it never touches the catalog's own tables. This is a **second
/// connection** into the same file the r2d2 pool serves — safe under WAL, the
/// established pattern (Perseus's `SeenStore` does the same).
///
/// The receiver uses it for the ack-replay guard ([`count_receipts`](Self::count_receipts)
/// / [`load_receipts`](Self::load_receipts)) and borrows its connection
/// ([`lock_conn`](Self::lock_conn)) to run the per-frame ingest transaction on
/// the very same connection that writes the receipt + history rows.
pub struct CatalogSyncStore {
    conn: Mutex<Connection>,
}

impl CatalogSyncStore {
    /// Open a WAL connection to the catalog at `path` and idempotently ensure the
    /// sync tables/indexes exist. Assumes the catalog schema itself is already
    /// initialised by [`init_db`](crate::db::schema::init_db).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let conn = Connection::open(path)
            .with_context(|| format!("open catalog sync store {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .context("configure catalog sync store pragmas")?;
        conn.execute(DDL_OUTBOUND, []).context("create sync_outbound")?;
        conn.execute(DDL_HISTORY, []).context("create sync_history")?;
        conn.execute(DDL_RECEIPTS, []).context("create sync_receipts")?;
        conn.execute(DDL_SYNC_SOURCES, []).context("create sync_sources")?;
        for idx in DDL_INDEXES {
            conn.execute(idx, []).context("create sync_history index")?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Borrow the underlying connection for a synchronous unit of work (the
    /// receiver's per-frame ingest transaction). The guard must never be held
    /// across an `.await` — ingest is fully synchronous, so it isn't.
    pub fn lock_conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("catalog sync store mutex poisoned")
    }

    /// Total receipts recorded for `package_id`, any outcome. See
    /// [`count_satisfied_receipts`](Self::count_satisfied_receipts) for the
    /// actual ack-replay guard.
    pub fn count_receipts(&self, package_id: &PackageId) -> Result<u32> {
        let conn = self.lock_conn();
        count_receipts(&conn, &package_id.0)
    }

    /// Number of non-`Rejected` receipts recorded for `package_id` — the
    /// ack-replay guard. A package is only "fully receipted" when this equals
    /// the announced `frame_count`; a pending `Rejected` receipt must never be
    /// counted as satisfied (it needs a redelivery, not a replay).
    pub fn count_satisfied_receipts(&self, package_id: &PackageId) -> Result<u32> {
        let conn = self.lock_conn();
        count_satisfied_receipts(&conn, &package_id.0)
    }

    /// Every receipt recorded for `package_id`, ready to replay in an ack.
    pub fn load_receipts(&self, package_id: &PackageId) -> Result<Vec<FrameReceipt>> {
        let conn = self.lock_conn();
        load_receipts(&conn, &package_id.0)
    }

    /// Every outbound row, newest-first, capped at `limit`. Twin of
    /// [`StandaloneSyncStore::all_outbound`] for symmetry — see
    /// [`all_outbound_rows`].
    pub fn all_outbound(&self, limit: u32) -> Result<Vec<OutboundRow>> {
        let conn = self.lock_conn();
        all_outbound_rows(&conn, limit)
    }
}

impl SyncStore for CatalogSyncStore {
    fn enqueue(&self, package_ref: &str, peer: NodeId) -> Result<i64> {
        let conn = self.lock_conn();
        conn.execute(
            "INSERT INTO sync_outbound (package_ref, peer, state, attempts, created_at)
             VALUES (?1, ?2, ?3, 0, ?4)",
            params![
                package_ref,
                node_id_hex(&peer),
                OutboundState::Queued.as_str(),
                now_iso()
            ],
        )
        .context("insert sync_outbound")?;
        Ok(conn.last_insert_rowid())
    }

    fn set_state(&self, id: i64, s: OutboundState) -> Result<()> {
        let conn = self.lock_conn();
        conn.execute(
            "UPDATE sync_outbound SET state = ?1 WHERE id = ?2",
            params![s.as_str(), id],
        )
        .with_context(|| format!("set state {} for outbound {id}", s.as_str()))?;
        Ok(())
    }

    fn bump_attempts(&self, id: i64) -> Result<u32> {
        let conn = self.lock_conn();
        conn.execute(
            "UPDATE sync_outbound SET attempts = attempts + 1 WHERE id = ?1",
            params![id],
        )
        .with_context(|| format!("bump attempts for outbound {id}"))?;
        let attempts: i64 = conn
            .query_row(
                "SELECT attempts FROM sync_outbound WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .with_context(|| format!("read attempts for outbound {id}"))?;
        Ok(attempts.max(0) as u32)
    }

    fn non_terminal(&self) -> Result<Vec<OutboundRow>> {
        let conn = self.lock_conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {OUTBOUND_COLS} FROM sync_outbound
                 WHERE state NOT IN ('confirmed', 'failed')
                 ORDER BY id ASC"
            ))
            .context("prepare non_terminal")?;
        let raws = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })
            .context("query non_terminal")?
            .collect::<rusqlite::Result<Vec<OutboundRaw>>>()
            .context("collect non_terminal")?;
        raws.into_iter().map(to_outbound).collect()
    }

    fn confirmed(&self) -> Result<Vec<OutboundRow>> {
        let conn = self.lock_conn();
        confirmed_outbound_rows(&conn)
    }

    fn confirm(&self, id: i64, receipts: &[FrameReceipt]) -> Result<()> {
        let conn = self.lock_conn();
        let changed = conn
            .execute(
                "UPDATE sync_outbound SET state = ?1, confirmed_at = ?2
                 WHERE id = ?3 AND state <> ?1",
                params![OutboundState::Confirmed.as_str(), now_iso(), id],
            )
            .with_context(|| format!("confirm outbound {id}"))?;
        tracing::debug!(package_id = id, receipts = receipts.len(), changed, "catalog sync store confirm");
        Ok(())
    }

    fn append_history(&self, h: HistoryRow) -> Result<()> {
        let conn = self.lock_conn();
        insert_history_row(&conn, &h)
    }

    fn search_history(&self, q: HistoryQuery) -> Result<Vec<HistoryRow>> {
        let conn = self.lock_conn();
        search_history_rows(&conn, &q)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEER: NodeId = [7u8; 32];

    /// `all_outbound` returns EVERY state (unlike `confirmed`), newest-id-first,
    /// and honours the `limit` cap.
    #[test]
    fn all_outbound_returns_every_state_newest_first_capped() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap();

        // Three packages in distinct states.
        let queued = store.enqueue("pkg-queued", PEER).unwrap();
        let transferring = store.enqueue("pkg-transferring", PEER).unwrap();
        let confirmed = store.enqueue("pkg-confirmed", PEER).unwrap();
        store.set_state(transferring, OutboundState::Transferring).unwrap();
        store.confirm(confirmed, &[]).unwrap();

        let rows = store.all_outbound(100).unwrap();
        assert_eq!(rows.len(), 3, "every row is returned regardless of state");
        // Newest id first.
        assert_eq!(rows[0].id, confirmed);
        assert_eq!(rows[1].id, transferring);
        assert_eq!(rows[2].id, queued);
        // States are preserved, including the non-confirmed ones `confirmed()` hides.
        assert_eq!(rows[0].state, OutboundState::Confirmed);
        assert_eq!(rows[1].state, OutboundState::Transferring);
        assert_eq!(rows[2].state, OutboundState::Queued);

        // The limit caps the result and keeps the newest rows.
        let capped = store.all_outbound(1).unwrap();
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].id, confirmed, "the cap keeps the newest rows");
    }

    /// An empty store yields an empty list, never an error.
    #[test]
    fn all_outbound_empty_store_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap();
        assert!(store.all_outbound(50).unwrap().is_empty());
    }
}
