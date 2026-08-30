//! Durable store for the sync engine.
//!
//! [`SyncStore`] is a **synchronous** (rusqlite) trait called from the engine's
//! worker task. Its SQL — DDL and every statement — lives here so both the
//! standalone implementation ([`StandaloneSyncStore`], this task) and the
//! catalog-backed one (`CatalogSyncStore`, task A7) share one schema. That later
//! implementation reuses [`DDL_OUTBOUND`] / [`DDL_HISTORY`] / [`DDL_INDEXES`]
//! verbatim from a `db/schema.rs` migration; A4 does not touch the app catalog.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};

use crate::sharing::types::{
    AnnounceFileEntry, FrameReceipt, NodeId, PackageId, PackageLayout, ReceiptOutcome,
};

use super::models::{
    Direction, HistoryQuery, HistoryRow, InboundFileRow, InboundFileState, InboundRow,
    InboundState, OutboundFileRow, OutboundFileState, OutboundRow, OutboundState, SyncEventRow,
};
use super::status::TransferFileCounts;
use super::{node_id_from_hex, node_id_hex, now_iso};

/// `sync_outbound` — the durable outbound state machine, one row per package.
///
/// The trailing `last_error` (Task 9), `next_retry_at` (Task 2),
/// `wire_package_id` (zombie-inbound fix), `display_name` (Transfers Status
/// Model v2 §D1) and `project_id` (Transfers Batch Model, spec §D6 — the collab
/// forward-compat key) columns are created inline for a fresh DB; an existing
/// table without them is migrated in place by [`ensure_outbound_columns`] (a
/// `CREATE TABLE IF NOT EXISTS` never adds a column).
pub const DDL_OUTBOUND: &str = "CREATE TABLE IF NOT EXISTS sync_outbound (
    id INTEGER PRIMARY KEY,
    package_ref TEXT NOT NULL,
    peer TEXT NOT NULL,
    state TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    confirmed_at TEXT,
    last_error TEXT,
    next_retry_at TEXT,
    wire_package_id TEXT,
    display_name TEXT,
    project_id TEXT,
    generation INTEGER NOT NULL DEFAULT 1,
    layout TEXT NOT NULL DEFAULT 'batch'
)";

/// Idempotently add the trailing `sync_outbound` columns (`last_error` — Task 9,
/// `next_retry_at` — Task 2, `wire_package_id` — zombie-inbound fix,
/// `display_name` — Transfers Status Model v2 §D1, `project_id` — Transfers Batch
/// Model §D6, `generation` — Transfers Batch Model §D5, `layout` — Perseus
/// mirror-hierarchy) to an existing table.
///
/// The guarded-ALTER twin of [`ensure_history_columns`]: `CREATE TABLE IF NOT
/// EXISTS` never alters an already-materialised table, so a store opened over a
/// pre-Task-9 / pre-Task-2 catalog / Perseus DB is migrated in place here. SQLite
/// has no `ADD COLUMN IF NOT EXISTS`, so this reads `PRAGMA table_info` once and
/// adds only the missing columns — the same idiom as [`ensure_history_columns`].
/// Called by every `sync_outbound` materialisation site (both store `open`s and
/// `db::schema::init_db`) right after the DDL. Never breaks an existing DB: an
/// already-present column is a no-op.
pub fn ensure_outbound_columns(conn: &Connection) -> Result<()> {
    let existing: HashSet<String> = conn
        .prepare("PRAGMA table_info(sync_outbound)")
        .context("prepare table_info(sync_outbound)")?
        .query_map([], |r| r.get::<_, String>(1))
        .context("query table_info(sync_outbound)")?
        .filter_map(|c| c.ok())
        .collect();
    for (col, ty) in [
        ("last_error", "TEXT"),
        ("next_retry_at", "TEXT"),
        ("wire_package_id", "TEXT"),
        ("display_name", "TEXT"),
        // Transfers Batch Model (spec §D6): the collab forward-compat key —
        // `NULL` for a personal transfer, the project id for a collab one.
        ("project_id", "TEXT"),
        // Transfers Batch Model (spec §D5): the user-facing "attempt N" generation
        // counter, bumped only by `reset_outbound_for_resend`. NOT NULL DEFAULT 1
        // is a constant default, so ALTER ADD COLUMN back-fills every existing row
        // to generation 1.
        ("generation", "INTEGER NOT NULL DEFAULT 1"),
        // Perseus mirror-hierarchy (spec 2026-07-27): receiver landing layout,
        // stamped per transfer at enqueue. Constant default back-fills 'batch'.
        ("layout", "TEXT NOT NULL DEFAULT 'batch'"),
    ] {
        if !existing.contains(col) {
            conn.execute(
                &format!("ALTER TABLE sync_outbound ADD COLUMN {col} {ty}"),
                [],
            )
            .with_context(|| format!("add sync_outbound.{col} column"))?;
        }
    }
    Ok(())
}

/// `sync_history` — append-only per-frame transfer audit log.
///
/// The trailing `project` column (Stage II collab, Task 11), `package_id`
/// column (per-batch detail, Task 14) and `batch_name` column (Transfers Status
/// Model v2 §D1) are created inline for a fresh DB; an existing table missing any
/// is migrated in place by [`ensure_history_columns`] (a `CREATE TABLE IF NOT
/// EXISTS` never adds a column).
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
    outcome TEXT,
    project TEXT,
    package_id TEXT,
    batch_name TEXT
)";

/// Idempotently add the trailing `project` (Task 11), `package_id` (Task 14) and
/// `batch_name` (Transfers Status Model v2 §D1) columns to an existing
/// `sync_history` table.
///
/// `CREATE TABLE IF NOT EXISTS` never alters an already-materialised table, so a
/// store opened over a pre-Stage-II / pre-Task-14 catalog / Perseus DB is migrated
/// in place here (AUDIT B3). SQLite has no `ADD COLUMN IF NOT EXISTS`, so this
/// reads `PRAGMA table_info` once and adds only the missing columns — the same
/// guarded-ALTER idiom as [`ensure_outbound_columns`]. Called by every
/// `sync_history` materialisation site (both store `open`s and
/// `db::schema::init_db`) right after the DDL. Never breaks an existing DB: an
/// already-present column is a no-op.
pub fn ensure_history_columns(conn: &Connection) -> Result<()> {
    let existing: HashSet<String> = conn
        .prepare("PRAGMA table_info(sync_history)")
        .context("prepare table_info(sync_history)")?
        .query_map([], |r| r.get::<_, String>(1))
        .context("query table_info(sync_history)")?
        .filter_map(|c| c.ok())
        .collect();
    for (col, ty) in [
        ("project", "TEXT"),
        ("package_id", "TEXT"),
        ("batch_name", "TEXT"),
    ] {
        if !existing.contains(col) {
            conn.execute(
                &format!("ALTER TABLE sync_history ADD COLUMN {col} {ty}"),
                [],
            )
            .with_context(|| format!("add sync_history.{col} column"))?;
        }
    }
    Ok(())
}

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

/// `sync_inbound` — the receiver's durable per-package receive state (Task 11).
///
/// One row per `(peer, package_id)`: the mirror of [`DDL_OUTBOUND`] for the
/// receive half. The receiver upserts a row to `announced` on every announce
/// (refreshing `frame_count`/`byte_size` and resetting a re-delivery back to
/// `announced` — but leaving a `cancelled` row untouched, since Cancelled is
/// final), advances it through `fetching`/`ingesting`, streams live `bytes_done`
/// during the fetch, and stamps a terminal `done`/`failed` at the end. It backs
/// the Transfers UI's Active/receive pane (Task 14). Like the other sync tables
/// the DDL lives here so `db/schema.rs` and both store `open`s share one
/// definition. `sync_inbound` shipped in beta.3 (Task 11), so a 0.4.x DB already
/// carries the table WITHOUT the trailing `display_name` column (Transfers Status
/// Model v2 §D1) or the `batch_uuid` / `project_id` columns (Transfers Batch Model
/// §D1/§D6); those columns are created inline for a fresh DB and back-filled on an
/// existing one by [`ensure_inbound_columns`].
///
/// `batch_uuid` is the batch-model row key: one long-lived row per transfer keyed
/// `(peer, batch_uuid)` across all attempts, enforced by the `idx_sync_inbound_batch`
/// unique index ([`DDL_INDEXES`]). The legacy `UNIQUE(peer, package_id)` constraint
/// stays — `package_id` is still per-attempt-unique (now "current attempt's wire
/// id"). Its presence feeds the batch-model upgrade detector
/// ([`transfer_wipe_needed`], driving [`wipe_transfer_bookkeeping_on_upgrade`]) —
/// see that fn's docs for the full two-shape detection (it also catches a
/// pre-Task-11 DB that lacks this table entirely).
pub const DDL_INBOUND: &str = "CREATE TABLE IF NOT EXISTS sync_inbound (
    id INTEGER PRIMARY KEY,
    peer TEXT NOT NULL,
    package_id TEXT NOT NULL,
    state TEXT NOT NULL,
    frame_count INTEGER NOT NULL DEFAULT 0,
    byte_size INTEGER NOT NULL DEFAULT 0,
    bytes_done INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL,
    finished_at TEXT,
    display_name TEXT,
    landing_dir TEXT,
    batch_uuid TEXT,
    project_id TEXT,
    generation INTEGER NOT NULL DEFAULT 1,
    declined_at TEXT,
    peer_capability TEXT,
    UNIQUE(peer, package_id)
)";

/// Idempotently add the trailing `sync_inbound` columns (`display_name` — Transfers
/// Status Model v2 §D1, `landing_dir` — §D2, `batch_uuid` / `project_id` — Transfers
/// Batch Model §D1/§D6) to an existing table.
///
/// The guarded-ALTER twin of [`ensure_outbound_columns`] / [`ensure_history_columns`]:
/// `sync_inbound` shipped in beta.3 without these columns, so a store opened over
/// a 0.4.x catalog / Perseus DB is migrated in place here. Called by every
/// `sync_inbound` materialisation site (both store `open`s and `db::schema::init_db`)
/// right after the DDL. [`wipe_transfer_bookkeeping_on_upgrade`] (and its detector,
/// [`transfer_wipe_needed`]) must run BEFORE this — indeed before any DDL in the
/// same pass — so its inspection of `batch_uuid`/table presence reflects the
/// pre-call state, not a shape this very call already created. Never breaks an
/// existing DB: an already-present column is a no-op.
pub fn ensure_inbound_columns(conn: &Connection) -> Result<()> {
    let existing: HashSet<String> = conn
        .prepare("PRAGMA table_info(sync_inbound)")
        .context("prepare table_info(sync_inbound)")?
        .query_map([], |r| r.get::<_, String>(1))
        .context("query table_info(sync_inbound)")?
        .filter_map(|c| c.ok())
        .collect();
    // One atomic unit: the ALTERs and the declined_at backfill commit together
    // (SQLite DDL is transactional), so a crash between "column added" and
    // "backfill ran" can never strand a half-migrated table — the next open
    // would otherwise see the column present and skip the backfill forever.
    conn.execute("SAVEPOINT ensure_inbound_cols", [])
        .context("open ensure_inbound_cols savepoint")?;
    let result = ensure_inbound_columns_inner(conn, &existing);
    if result.is_ok() {
        conn.execute("RELEASE ensure_inbound_cols", [])
            .context("release ensure_inbound_cols savepoint")?;
    } else {
        // Best-effort unwind; the original error is the one worth surfacing.
        let _ = conn.execute("ROLLBACK TO ensure_inbound_cols", []);
        let _ = conn.execute("RELEASE ensure_inbound_cols", []);
    }
    result
}

/// The migration body of [`ensure_inbound_columns`], run inside its savepoint.
fn ensure_inbound_columns_inner(conn: &Connection, existing: &HashSet<String>) -> Result<()> {
    for (col, ty) in [
        ("display_name", "TEXT"),
        ("landing_dir", "TEXT"),
        // Transfers Batch Model: `batch_uuid` is the durable per-transfer key
        // (unique `(peer, batch_uuid)` via `idx_sync_inbound_batch`); `project_id`
        // is the collab forward-compat key (§D6).
        ("batch_uuid", "TEXT"),
        ("project_id", "TEXT"),
        // Transfers Batch Model (spec §D5): the user-facing "attempt N" generation
        // counter, bumped only by `upsert_inbound_attempt`'s existing-row reset.
        // NOT NULL DEFAULT 1 is a constant default, so ALTER ADD COLUMN back-fills
        // every existing row to generation 1.
        ("generation", "INTEGER NOT NULL DEFAULT 1"),
        // Decline Finality Axis (spec §D1): non-NULL ⇔ the receiving user declined
        // this transfer — the transfer-level finality marker, independent of the
        // attempt-level `state` column (which sender revokes / restart reconcile /
        // epilogues legitimately overwrite).
        ("declined_at", "TEXT"),
        // Perseus UI v2 (Task 9): the announcing peer's device capability
        // (`"athenaeum"` / `"perseus"`), stamped at announce time from the cached
        // device-list capability map so it survives a later device revocation.
        // Informational only — never gates a transfer.
        ("peer_capability", "TEXT"),
    ] {
        if !existing.contains(col) {
            conn.execute(
                &format!("ALTER TABLE sync_inbound ADD COLUMN {col} {ty}"),
                [],
            )
            .with_context(|| format!("add sync_inbound.{col} column"))?;
        }
    }
    // One-shot backfill, only when `declined_at` was just created (spec §D1): on a
    // pre-axis DB the only `cancelled` rows with `last_error` equal to
    // [`REVOKED_BY_SENDER_DETAIL`] are sender revokes (must NOT become declines);
    // every other `cancelled` row is a genuine receiver decline. One canonical
    // statement — `created_at`/`finished_at`/`last_error` are in the base DDL of
    // every real shape, so there is deliberately NO column-tolerant fallback (a
    // silently degraded predicate here would reclassify revokes as declines).
    if !existing.contains("declined_at") {
        conn.execute(
            "UPDATE sync_inbound SET declined_at = COALESCE(finished_at, created_at)
             WHERE state = 'cancelled'
               AND (last_error IS NULL OR last_error <> ?1)",
            params![crate::sync::models::REVOKED_BY_SENDER_DETAIL],
        )
        .context("backfill sync_inbound.declined_at")?;
    }
    Ok(())
}

/// The eight transfer-bookkeeping tables the batch-model upgrade wipes
/// (spec §Migration). Order is irrelevant — the store declares no FKs between
/// them. A sender-only store ([`StandaloneSyncStore`] / Perseus) never materialises
/// `sync_receipts` / `sync_sources`, so the wipe skips any table not present.
const TRANSFER_TABLES: [&str; 8] = [
    "sync_outbound",
    "sync_inbound",
    "sync_outbound_files",
    "sync_inbound_files",
    "sync_events",
    "sync_receipts",
    "sync_sources",
    "sync_history",
];

/// True iff `table` has a column named `col` (via `PRAGMA table_info`). Returns
/// `false` for a table that does not exist (empty pragma) — deliberately, so the
/// batch-upgrade detector reads a not-yet-materialised `sync_inbound` as "no
/// batch_uuid".
fn column_present(conn: &Connection, table: &str, col: &str) -> Result<bool> {
    let names: HashSet<String> = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .with_context(|| format!("prepare table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))
        .with_context(|| format!("query table_info({table})"))?
        .filter_map(|c| c.ok())
        .collect();
    Ok(names.contains(col))
}

/// True iff a table named `table` exists in this schema.
fn table_present(conn: &Connection, table: &str) -> Result<bool> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |r| r.get(0),
        )
        .with_context(|| format!("check table {table} exists"))?;
    Ok(n > 0)
}

/// Detect whether [`wipe_transfer_bookkeeping_on_upgrade`] must run — the
/// batch-model upgrade detector.
///
/// **Must be evaluated BEFORE any of the caller's own DDL executes this pass.**
/// Every sync-table `CREATE TABLE` is `IF NOT EXISTS`, so it un-conditionally
/// materialises the table; checking table/column presence any later can no
/// longer distinguish "freshly created by this very call" (fresh DB) from
/// "pre-existing with legacy rows" (an upgrade). This fn is pure inspection
/// (`PRAGMA`/`sqlite_master` reads) — it never creates anything, so calling it
/// first is always safe.
///
/// Two DB shapes both signal "wipe needed":
/// - **`sync_inbound` exists but lacks `batch_uuid`** — the beta.3-through-0.4.x
///   shape (Task 11 shipped `sync_inbound` without it).
/// - **`sync_inbound` does not exist at all, but `sync_outbound` or
///   `sync_history` does** — the PRE-Task-11 beta.1/beta.2 shape.
///   `sync_inbound` shipped only in beta.3, so an observatory/device still on
///   beta.1/beta.2 has neither the table nor the column, yet may already carry
///   `sync_outbound`/`sync_history` rows (including non-terminal outbound rows
///   [`resurrect_pending_senders`](super::engine) would otherwise pick back up).
///   Column-presence alone reads this shape as "already migrated" (wrong) once
///   `DDL_INBOUND` has created a fresh, columned `sync_inbound` earlier in the
///   same call — hence the BEFORE-any-DDL requirement above.
///
/// A truly fresh DB has NEITHER `sync_inbound` NOR `sync_outbound` NOR
/// `sync_history` yet (nothing this call's own DDL has touched) → `false`,
/// guaranteed no-op — never skipped by flag-purity alone.
fn transfer_wipe_needed(conn: &Connection) -> Result<bool> {
    if table_present(conn, "sync_inbound")? {
        return Ok(!column_present(conn, "sync_inbound", "batch_uuid")?);
    }
    Ok(table_present(conn, "sync_outbound")? || table_present(conn, "sync_history")?)
}

/// One-shot **clean reset** of ALL transfer bookkeeping on the batch-model upgrade
/// (spec §Migration, owner decision 2026-07-21: the accumulated records are
/// smoke-test debris; preserving them buys nothing).
///
/// Gated by [`transfer_wipe_needed`] — call THIS fn (and thus that detector) at
/// the very top of the sync-table materialisation block, before any
/// `conn.execute(DDL_*)` in this pass, so the detector sees the true pre-call
/// state (see its docs for why order matters). The wipe itself only needs tables
/// to ALREADY exist from a prior run — `DELETE FROM` does not care about a
/// table's exact column shape — so running before this call's own DDL is safe:
/// a table this call is about to create fresh is, by definition, not present yet
/// and is skipped by the [`table_present`] guard below (nothing to delete).
///
/// The wipe runs in its OWN transaction: the caller (either store `open` or
/// `init_db`) holds no open transaction on this connection, matching the
/// `unchecked_transaction` seam discipline of the other store fns. One tx makes
/// the wipe all-or-nothing.
///
/// Catalog tables (`files` / `frames` / …) are NEVER named — untouched by
/// construction. A sender-only store lacks `sync_receipts` / `sync_sources`; each
/// DELETE is guarded by [`table_present`] so a missing table is skipped, never an
/// error. `info!` records the per-table deleted count.
pub fn wipe_transfer_bookkeeping_on_upgrade(conn: &Connection) -> Result<()> {
    if !transfer_wipe_needed(conn)? {
        return Ok(());
    }
    let tx = conn
        .unchecked_transaction()
        .context("begin transfer-bookkeeping wipe")?;
    for table in TRANSFER_TABLES {
        if !table_present(&tx, table)? {
            continue;
        }
        let deleted = tx
            .execute(&format!("DELETE FROM {table}"), [])
            .with_context(|| format!("wipe {table}"))?;
        tracing::info!(
            table,
            count = deleted,
            "batch-model upgrade: wiped transfer table"
        );
    }
    tx.commit().context("commit transfer-bookkeeping wipe")?;
    Ok(())
}

/// `sync_outbound_files` — the sender's durable per-file state within a batch
/// (Transfers Status Model v2 §D4). One row per `(outbound_id, rel_path)`, created
/// at package-build time and transitioned `pending → sending → uploaded → done`
/// as the send progresses. `bytes_done` is checkpointed on transitions + serve
/// ticks so the per-file picture restores after a restart/resume; `outcome`
/// records the receiver's per-frame verdict (same text encoding as
/// [`DDL_RECEIPTS`]) once the ack lands. A NEW table — `CREATE TABLE IF NOT
/// EXISTS` alone materialises it (no legacy shape to migrate).
pub const DDL_OUTBOUND_FILES: &str = "CREATE TABLE IF NOT EXISTS sync_outbound_files (
    outbound_id INTEGER NOT NULL,
    rel_path TEXT NOT NULL,
    byte_size INTEGER NOT NULL,
    frame_uuid TEXT NOT NULL,
    state TEXT NOT NULL,
    bytes_done INTEGER NOT NULL DEFAULT 0,
    outcome TEXT,
    error TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (outbound_id, rel_path)
)";

/// `sync_inbound_files` — the receiver's durable per-file state within a batch
/// (Transfers Status Model v2 §D4). One row per `(inbound_id, rel_path)`, created
/// AT ANNOUNCE TIME from the v2 manifest and transitioned `announced → fetching →
/// done` (or `failed`) as the fetch/ingest progresses. Mirror of
/// [`DDL_OUTBOUND_FILES`]; a NEW table with no legacy shape.
pub const DDL_INBOUND_FILES: &str = "CREATE TABLE IF NOT EXISTS sync_inbound_files (
    inbound_id INTEGER NOT NULL,
    rel_path TEXT NOT NULL,
    byte_size INTEGER NOT NULL,
    frame_uuid TEXT NOT NULL,
    state TEXT NOT NULL,
    bytes_done INTEGER NOT NULL DEFAULT 0,
    outcome TEXT,
    error TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (inbound_id, rel_path)
)";

/// `sync_events` — the capped per-batch event journal (Transfers Status Model v2
/// §D7, the detail pane's **Log** tab). Append-only from the caller's side, but
/// [`append_sync_event`] prunes each insert down to the newest ~200 rows per
/// `(direction, batch_key)` so a long-lived batch never grows the table without
/// bound. `batch_key` is the outbound row id (sent) or inbound row id (received),
/// stored as text. A NEW table — `CREATE TABLE IF NOT EXISTS` alone materialises
/// it.
pub const DDL_SYNC_EVENTS: &str = "CREATE TABLE IF NOT EXISTS sync_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    direction TEXT NOT NULL,
    batch_key TEXT NOT NULL,
    ts TEXT NOT NULL,
    kind TEXT NOT NULL,
    detail TEXT
)";

/// `sync_sources` — **VESTIGIAL** since 2026-08-29.
///
/// It used to hold the app sender's `sync_outbound.package_ref` → source-file
/// linkage, written at package-build time so the app-shell retention loop could
/// resolve a confirmed package back to the disk files it might reclaim. That
/// loop is gone (owner ruling: retention is a Perseus-only concern — the app
/// never deletes a sent source), so NOTHING writes or reads this table any
/// more. The DDL and its entry in `TRANSFER_TABLES` stay so existing catalogs
/// keep a table they already have and the upgrade wipe still finds it; dropping
/// it is a future schema pass.
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

/// Search indexes for [`SyncStore::search_history`], the per-batch event-log
/// index (Transfers Status Model v2 §D7) that keeps [`list_sync_events`] /
/// [`append_sync_event`]'s `(direction, batch_key)`-scoped, id-ordered scans fast,
/// and the Transfers Batch Model unique `(peer, batch_uuid)` index that anchors the
/// receiver's one-row-per-transfer key (spec §D1). Iterated at every sync-table
/// materialisation site, so a new entry here is created in the Perseus store, the
/// catalog store, and `db::schema::init_db` alike.
///
/// `idx_sync_inbound_batch` is UNIQUE but tolerates the multiple-NULL-`batch_uuid`
/// rows a v1/v2 (legacy-announce) receive still writes — SQLite treats NULLs as
/// distinct in a unique index — so it constrains only the batch-keyed (v3) rows.
/// It is created LAST (after [`ensure_inbound_columns`] has added `batch_uuid`).
pub const DDL_INDEXES: [&str; 5] = [
    "CREATE INDEX IF NOT EXISTS idx_sync_history_filename ON sync_history(filename)",
    "CREATE INDEX IF NOT EXISTS idx_sync_history_object ON sync_history(object)",
    "CREATE INDEX IF NOT EXISTS idx_sync_history_started_at ON sync_history(started_at)",
    "CREATE INDEX IF NOT EXISTS idx_sync_events_batch ON sync_events(direction, batch_key, id)",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_sync_inbound_batch ON sync_inbound(peer, batch_uuid)",
];

/// Durable persistence for the outbound state machine + transfer history.
///
/// Synchronous by contract — the engine worker calls these directly. Every
/// implementation is `Send + Sync` so it can live behind an `Arc<dyn SyncStore>`
/// shared by the worker and the [`SyncEngineHandle`](super::engine::SyncEngineHandle).
pub trait SyncStore: Send + Sync {
    /// Insert a new package in [`Queued`](OutboundState::Queued), together with its
    /// `display_name` and per-file `pending` rows, in ONE transaction; returns its
    /// id. `display_name`/`files` may be `None`/empty (Perseus, a nameless retry).
    /// See [`insert_outbound_with_files`] for the race-kill rationale (Transfers
    /// Status Model v2 §D1/§D4).
    ///
    /// `layout` is the transfer's landing shape (mirror-hierarchy T2), stamped in
    /// the same transaction as the row.
    fn enqueue(
        &self,
        package_ref: &str,
        peer: NodeId,
        display_name: Option<&str>,
        files: &[AnnounceFileEntry],
        layout: PackageLayout,
    ) -> Result<i64>;

    /// Replace the whole per-file row set for an outbound batch (§D4). Delegates
    /// to [`replace_outbound_files`]; on the trait so a future task can rebuild a
    /// batch's file manifest through the engine's store handle.
    fn replace_outbound_files(&self, outbound_id: i64, files: &[OutboundFileRow]) -> Result<()>;

    /// Every per-file row for an outbound batch, ordered by `rel_path` (§D4).
    /// Delegates to [`list_outbound_files`]. The engine reads it to settle the
    /// un-acked rows on a terminal `cancelled`/`failed`.
    fn list_outbound_files(&self, outbound_id: i64) -> Result<Vec<OutboundFileRow>>;

    /// Update one outbound file's mutable fields (§D4). Delegates to
    /// [`set_outbound_file_state`]. Where the UPDATE matches 0 rows (an unknown
    /// `rel_path` — e.g. a served `manifest.ndjson`, or a Perseus batch with no
    /// per-file rows) the implementation logs `debug!` rather than erroring.
    fn set_outbound_file_state(
        &self,
        outbound_id: i64,
        rel_path: &str,
        state: OutboundFileState,
        bytes_done: u64,
        outcome: Option<&str>,
        error: Option<&str>,
    ) -> Result<()>;

    /// Append one event to this batch's journal, pruned to the newest
    /// [`SYNC_EVENTS_CAP`] (§D7). Delegates to [`append_sync_event`]; the engine
    /// records the outbound lifecycle here (`enqueued`, `announce_sent`, …).
    fn append_sync_event(
        &self,
        direction: Direction,
        batch_key: &str,
        kind: &str,
        detail: Option<&str>,
    ) -> Result<()>;

    /// Force one outbound row to a new state.
    fn set_state(&self, id: i64, s: OutboundState) -> Result<()>;

    /// Record the most recent failed-attempt reason for a package (Task 9), or
    /// clear it with `None` on success. Best-effort diagnostic surfaced beside
    /// `attempts` on the Perseus status page; [`confirm`](Self::confirm) also clears
    /// it, so a `confirmed` row never carries a stale error.
    fn set_last_error(&self, id: i64, err: Option<&str>) -> Result<()>;

    /// Persist (or clear with `None`) the wall-clock deadline of the next retry
    /// attempt (Task 2). The engine writes it every time it arms a `Retry`
    /// deadline so the UI can render a live countdown and a restart re-arms
    /// honestly; it clears it on a successful announce (now awaiting the ack) and
    /// on every terminal transition. Best-effort from the engine's side — a
    /// failed persist is logged and never breaks scheduling.
    fn set_next_retry_at(&self, id: i64, at: Option<&str>) -> Result<()>;

    /// Persist the stable wire `package_id` minted for this row's announce
    /// (zombie-inbound fix). Minted ONCE by the engine on the first successful
    /// announce build and reused on every crash-resume re-announce so the wire id
    /// is stable across sender restarts — the receiver's `sync_inbound` row (keyed
    /// on the wire id) is reused instead of orphaned when the sender restarts
    /// mid-transfer. A re-enqueue (retry) is a NEW row with a NULL value here and
    /// so mints a fresh id (deliberate — receiver receipts are keyed by the wire
    /// id, so a resend must transfer anew rather than replay-confirm from stale
    /// receipts). Best-effort from the engine's side — a failed persist is logged
    /// and never breaks the send (a fresh id would just be minted next restart).
    fn set_wire_package_id(&self, id: i64, wire_id: &str) -> Result<()>;

    /// Increment `attempts` and return the new value (drives max-attempts).
    fn bump_attempts(&self, id: i64) -> Result<u32>;

    /// Every non-terminal outbound row — the crash-resume enumeration.
    fn non_terminal(&self) -> Result<Vec<OutboundRow>>;

    /// Fetch a single outbound row by id (`None` if absent). The terminal-state
    /// read behind the retry command surface (Task 8) and the Perseus retry path;
    /// promoted from a [`StandaloneSyncStore`] inherent into the trait so the
    /// catalog-backed store exposes it too. Delegates to [`outbound_row_by_id`].
    fn get_outbound(&self, id: i64) -> Result<Option<OutboundRow>>;

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

    /// Reset an outbound transfer for a **resend attempt** — the SAME row,
    /// `attempts += 1`, fresh `wire_package_id`, cleared error/retry/confirm fields,
    /// and every per-file row back to `pending` (Transfers Batch Model, §D1/§D3).
    /// The bridge B3's engine drives resend through (over `Arc<dyn SyncStore>`);
    /// delegates to the free [`reset_outbound_for_resend`]. Errors on an absent row;
    /// terminal-state eligibility is the caller's policy.
    fn reset_outbound_for_resend(&self, id: i64, new_wire_id: &str) -> Result<()>;

    /// Stamp the collab `project_id` on an outbound row (spec §D6). Delegates to
    /// the free [`set_outbound_project_id`]. The engine calls it best-effort at the
    /// enqueue path for a project package (a personal send never calls it — no
    /// stamp). No behavioral use this wave; the column just distinguishes collab
    /// rows for the future planner.
    ///
    /// Default is a no-op so a transport-less / personal-only store
    /// (Perseus's mock + its decorator, which never handle project packages) needs
    /// no override; the two real catalog/standalone stores back the only rows that
    /// ever carry a project stamp and override it.
    fn set_outbound_project_id(&self, id: i64, project_id: Option<&str>) -> Result<()> {
        let _ = (id, project_id);
        Ok(())
    }

    /// Append one audit row.
    fn append_history(&self, h: HistoryRow) -> Result<()>;

    /// Exact-match, newest-first history search (see [`HistoryQuery`]).
    fn search_history(&self, q: HistoryQuery) -> Result<Vec<HistoryRow>>;
}

/// Raw column tuple for a `sync_outbound` row, parsed into [`OutboundRow`] by
/// [`to_outbound`] so fallible text parsing happens outside the rusqlite closure.
type OutboundRaw = (
    i64,
    String,
    String,
    String,
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    String,
);

fn to_outbound(raw: OutboundRaw) -> Result<OutboundRow> {
    let (
        id,
        package_ref,
        peer_hex,
        state,
        attempts,
        created_at,
        confirmed_at,
        last_error,
        next_retry_at,
        wire_package_id,
        display_name,
        project_id,
        generation,
        layout,
    ) = raw;
    Ok(OutboundRow {
        id,
        package_ref,
        peer: node_id_from_hex(&peer_hex)?,
        state: OutboundState::from_db(&state)?,
        attempts: attempts.max(0) as u32,
        created_at,
        confirmed_at,
        last_error,
        next_retry_at,
        wire_package_id,
        display_name,
        project_id,
        generation: generation.max(0) as u32,
        layout: PackageLayout::from_db(&layout),
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
    Option<String>,
    Option<String>,
    Option<String>,
);

fn to_history(raw: HistoryRaw) -> Result<HistoryRow> {
    let (
        frame_uuid,
        filename,
        object,
        peer_device,
        direction,
        bytes,
        started_at,
        finished_at,
        outcome,
        project,
        package_id,
        batch_name,
    ) = raw;
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
        project,
        package_id,
        batch_name,
    })
}

const OUTBOUND_COLS: &str =
    "id, package_ref, peer, state, attempts, created_at, confirmed_at, last_error, next_retry_at, wire_package_id, display_name, project_id, generation, layout";
const HISTORY_COLS: &str =
    "frame_uuid, filename, object, peer_device, direction, bytes, started_at, finished_at, outcome, project, package_id, batch_name";

/// Decode one `OutboundRaw` tuple from a query row — the SINGLE place the
/// `OUTBOUND_COLS` column order is read, shared by every outbound read path so a
/// new trailing column is threaded through exactly once (`display_name` was).
fn outbound_raw_from_row(r: &rusqlite::Row) -> rusqlite::Result<OutboundRaw> {
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
        r.get(9)?,
        r.get(10)?,
        r.get(11)?,
        r.get(12)?,
        r.get(13)?,
    ))
}

/// Decode one `HistoryRaw` tuple from a query row (the single `HISTORY_COLS` read point).
fn history_raw_from_row(r: &rusqlite::Row) -> rusqlite::Result<HistoryRaw> {
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
        r.get(9)?,
        r.get(10)?,
        r.get(11)?,
    ))
}

/// Decode one `InboundRaw` tuple from a query row (the single `INBOUND_COLS` read point).
fn inbound_raw_from_row(r: &rusqlite::Row) -> rusqlite::Result<InboundRaw> {
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
        r.get(9)?,
        r.get(10)?,
        r.get(11)?,
        r.get(12)?,
        r.get(13)?,
        r.get(14)?,
        r.get(15)?,
        r.get(16)?,
    ))
}

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
         (frame_uuid, filename, object, peer_device, direction, bytes, started_at, finished_at, outcome, project, package_id, batch_name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
            h.project,
            h.package_id,
            h.batch_name,
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
    if let Some(project) = &q.project {
        sql.push_str(" AND project = ?");
        args.push(Box::new(project.clone()));
    }
    if let Some(package_id) = &q.package_id {
        sql.push_str(" AND package_id = ?");
        args.push(Box::new(package_id.clone()));
    }
    sql.push_str(" ORDER BY started_at DESC, id DESC LIMIT ?");
    args.push(Box::new(q.limit as i64));

    let params_ref: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).context("prepare search_history")?;
    let raws = stmt
        .query_map(params_ref.as_slice(), history_raw_from_row)
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
        .query_map([], outbound_raw_from_row)
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
        .query_map(params![limit], outbound_raw_from_row)
        .context("query all_outbound_rows")?
        .collect::<rusqlite::Result<Vec<OutboundRaw>>>()
        .context("collect all_outbound_rows")?;
    raws.into_iter().map(to_outbound).collect()
}

/// Load `sync_outbound` rows in a TERMINAL state (`confirmed`/`failed`/
/// `cancelled`), newest-first, capped at `limit`. The restart-survival companion
/// to [`SyncStore::non_terminal`](SyncStore::non_terminal): `get_sync_status`
/// returns only the non-terminal in-flight rows, so a settled send (which the
/// user can still **Resend** if it failed/was cancelled) would vanish from the
/// Transfers list after a relaunch — taking its durable row id, and thus the
/// Resend affordance and the Files/Log detail, with it. This read resurrects the
/// recent terminal window straight from the durable table (tv2 follow-up).
///
/// Ordered by the finished timestamp where present (`confirmed_at`), else
/// `created_at`, then `id DESC` — newest-first — so the most recently settled
/// batches come back first; `LIMIT` bounds the response to the recent window
/// (not a pagination surface).
pub fn terminal_outbound(conn: &Connection, limit: u32) -> Result<Vec<OutboundRow>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {OUTBOUND_COLS} FROM sync_outbound
             WHERE state IN ('confirmed', 'failed', 'cancelled')
             ORDER BY COALESCE(confirmed_at, created_at) DESC, id DESC
             LIMIT ?1"
        ))
        .context("prepare terminal_outbound")?;
    let raws = stmt
        .query_map(params![limit], outbound_raw_from_row)
        .context("query terminal_outbound")?
        .collect::<rusqlite::Result<Vec<OutboundRaw>>>()
        .context("collect terminal_outbound")?;
    raws.into_iter().map(to_outbound).collect()
}

/// Fetch a single `sync_outbound` row by id (`None` if absent). Defined once here
/// so both store implementations share the by-id read verbatim; it backs
/// [`SyncStore::get_outbound`] — the terminal-state read the retry command surface
/// (Task 8) and the Perseus retry path rely on.
pub fn outbound_row_by_id(conn: &Connection, id: i64) -> Result<Option<OutboundRow>> {
    let raw = conn
        .query_row(
            &format!("SELECT {OUTBOUND_COLS} FROM sync_outbound WHERE id = ?1"),
            params![id],
            outbound_raw_from_row,
        )
        .optional()
        .context("query sync_outbound by id")?;
    raw.map(to_outbound).transpose()
}

/// Reset an outbound transfer for a **resend attempt** — the SAME row is reused,
/// never a new one (Transfers Batch Model, §D1/§D3). In ONE transaction:
///
/// - the `sync_outbound` row → state `queued`, `attempts += 1`,
///   `generation += 1` (the user-facing "attempt N" counter — §D5),
///   `wire_package_id = new_wire_id`, and `last_error` / `next_retry_at` /
///   `confirmed_at` cleared;
/// - EVERY one of the row's `sync_outbound_files` rows → state `pending`,
///   `bytes_done 0`, `outcome NULL`, `error NULL` (a fresh per-file picture; the
///   previous attempt's verdicts already live in `sync_history`).
///
/// A **fresh per-attempt wire id** is the ack-replay isolation: receipts stay
/// keyed by per-attempt wire ids, so a prior attempt's receipts can never replay
/// onto this one (§D1). `generation` increments so the UI shows "attempt N" —
/// `attempts` also bumps but ALSO counts the engine's internal announce-retries,
/// so it is NOT the user-facing counter (§D5).
///
/// **Errors** if `id` names no row — the caller must have loaded it. The mechanism
/// is unconditional: **terminal-state eligibility** (only a `failed` / `cancelled`
/// transfer may resend) is the CALLER's policy — B3's engine / B5's command guard
/// own it, this fn does not re-check state.
///
/// Its own transaction — the caller holds no open tx on this connection (the
/// `unchecked_transaction` seam discipline shared by [`replace_outbound_files`] et
/// al.).
pub fn reset_outbound_for_resend(conn: &Connection, id: i64, new_wire_id: &str) -> Result<()> {
    let tx = conn
        .unchecked_transaction()
        .context("begin reset_outbound_for_resend")?;
    let changed = tx
        .execute(
            "UPDATE sync_outbound
             SET state = ?1, attempts = attempts + 1, generation = generation + 1,
                 wire_package_id = ?2, last_error = NULL, next_retry_at = NULL,
                 confirmed_at = NULL
             WHERE id = ?3",
            params![OutboundState::Queued.as_str(), new_wire_id, id],
        )
        .with_context(|| format!("reset outbound {id} for resend"))?;
    if changed == 0 {
        return Err(anyhow!(
            "reset_outbound_for_resend: no sync_outbound row with id {id}"
        ));
    }
    tx.execute(
        "UPDATE sync_outbound_files
         SET state = ?1, bytes_done = 0, outcome = NULL, error = NULL, updated_at = ?2
         WHERE outbound_id = ?3",
        params![OutboundFileState::Pending.as_str(), now_iso(), id],
    )
    .with_context(|| format!("reset sync_outbound_files for {id}"))?;
    tx.commit().context("commit reset_outbound_for_resend")?;
    Ok(())
}

/// Stamp (or clear with `None`) the collab `project_id` on an outbound row (spec
/// §D6). The engine writes it at the store enqueue path from the package's
/// manifest stamp so collab transfers are distinguishable from personal ones; a
/// personal send never carries a stamp and this is not called. No behavioral use
/// this wave — the column is the collab planner's future key. A missing row is a
/// benign no-op (the caller is best-effort).
pub fn set_outbound_project_id(conn: &Connection, id: i64, project_id: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE sync_outbound SET project_id = ?1 WHERE id = ?2",
        params![project_id, id],
    )
    .with_context(|| format!("set project_id for outbound {id}"))?;
    Ok(())
}

/// Stable text encoding of a [`ReceiptOutcome`] for the `sync_receipts.outcome`
/// column: `ingested` / `duplicate` / `cancelled` / `rejected:<reason>`.
pub fn receipt_outcome_to_db(o: &ReceiptOutcome) -> String {
    match o {
        ReceiptOutcome::Ingested => "ingested".to_string(),
        ReceiptOutcome::Duplicate => "duplicate".to_string(),
        ReceiptOutcome::Cancelled => "cancelled".to_string(),
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
        // A deliberate receiver "no" — decoded as a first-class `Cancelled`, NOT
        // via the `rejected:` catch-all below (which would strand it as a pending
        // rejection). Kept ABOVE the catch-all so `cancelled` never falls through.
        "cancelled" => ReceiptOutcome::Cancelled,
        other => {
            ReceiptOutcome::Rejected(other.strip_prefix("rejected:").unwrap_or(other).to_string())
        }
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
        params![
            package_id,
            r.frame_uuid,
            r.xxh3,
            receipt_outcome_to_db(&r.outcome),
            received_at
        ],
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

// ── Inbound (receive-side) package rows ─────────────────────────────────────
//
// Free fns over a `&Connection` so both the receiver (via
// [`CatalogSyncStore::lock_conn`]) and any store impl share one source. Keyed by
// `package_id` (a per-announce UUID — unique in practice); the `(peer,
// package_id)` UNIQUE constraint anchors the upsert.

const INBOUND_COLS: &str =
    "id, peer, package_id, state, frame_count, byte_size, bytes_done, last_error, created_at, finished_at, display_name, landing_dir, batch_uuid, project_id, generation, declined_at, peer_capability";

/// Raw column tuple for a `sync_inbound` row, parsed into [`InboundRow`] by
/// [`to_inbound`] so fallible text parsing happens outside the rusqlite closure.
type InboundRaw = (
    i64,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
);

fn to_inbound(raw: InboundRaw) -> Result<InboundRow> {
    let (
        id,
        peer,
        package_id,
        state,
        frame_count,
        byte_size,
        bytes_done,
        last_error,
        created_at,
        finished_at,
        display_name,
        landing_dir,
        batch_uuid,
        project_id,
        generation,
        declined_at,
        peer_capability,
    ) = raw;
    Ok(InboundRow {
        id,
        peer,
        package_id,
        state: InboundState::from_db(&state)?,
        frame_count: frame_count.max(0) as u32,
        byte_size: byte_size.max(0) as u64,
        bytes_done: bytes_done.max(0) as u64,
        last_error,
        created_at,
        finished_at,
        display_name,
        landing_dir,
        batch_uuid,
        project_id,
        generation: generation.max(0) as u32,
        declined_at,
        peer_capability,
    })
}

/// RETIRED from the production announce path (the receiver keys on
/// [`upsert_inbound_attempt`] since the Batch Model) — kept as a test seeder.
/// NB its `cancelled`-is-final CASE guard encodes the PRE-axis semantics; live
/// finality is `declined_at` (Decline Finality Axis §D3), so don't pin new
/// decline behavior through this helper.
///
/// Upsert the inbound row for `(peer_hex, package_id)` to `announced`, returning
/// its durable id. A first announce inserts a fresh `announced` row; a
/// re-announce (redelivery) refreshes `frame_count`/`byte_size`, resets the state
/// back to `announced`, and clears `bytes_done`/`last_error`/`finished_at` so the
/// row honestly reflects a restarting fetch — **except** when the existing row is
/// `cancelled`, which is left completely untouched (the caller checks its state
/// before deciding to fetch). See [`DDL_INBOUND`].
pub fn upsert_inbound_announced(
    conn: &Connection,
    peer_hex: &str,
    package_id: &str,
    frame_count: u32,
    byte_size: u64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO sync_inbound (peer, package_id, state, frame_count, byte_size, bytes_done, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)
         ON CONFLICT(peer, package_id) DO UPDATE SET
             frame_count = CASE WHEN sync_inbound.state = 'cancelled' THEN sync_inbound.frame_count ELSE excluded.frame_count END,
             byte_size   = CASE WHEN sync_inbound.state = 'cancelled' THEN sync_inbound.byte_size   ELSE excluded.byte_size   END,
             state       = CASE WHEN sync_inbound.state = 'cancelled' THEN sync_inbound.state       ELSE 'announced'        END,
             bytes_done  = CASE WHEN sync_inbound.state = 'cancelled' THEN sync_inbound.bytes_done  ELSE 0                   END,
             last_error  = CASE WHEN sync_inbound.state = 'cancelled' THEN sync_inbound.last_error  ELSE NULL                END,
             finished_at = CASE WHEN sync_inbound.state = 'cancelled' THEN sync_inbound.finished_at ELSE NULL                END",
        params![peer_hex, package_id, InboundState::Announced.as_str(), frame_count, byte_size as i64, now_iso()],
    )
    .context("upsert sync_inbound")?;
    // `last_insert_rowid()` is NOT the conflicting row's id on an ON CONFLICT
    // UPDATE, so read the id back by its unique key.
    let id: i64 = conn
        .query_row(
            "SELECT id FROM sync_inbound WHERE peer = ?1 AND package_id = ?2",
            params![peer_hex, package_id],
            |r| r.get(0),
        )
        .context("read sync_inbound id")?;
    Ok(id)
}

/// Force the inbound row for `package_id` to `state`, writing `last_error` (or
/// clearing it with `None`). A terminal state ([`InboundState::is_terminal`])
/// also stamps `finished_at = now`; a non-terminal transition leaves it NULL.
/// Keyed by `package_id` alone, NOT the table's `UNIQUE(peer, package_id)` pair
/// — safe only because `package_id` is a per-announce v4 UUID, effectively
/// globally unique in practice; a hypothetical future path that reuses a
/// `package_id` across two different peers would need the peer too.
pub fn set_inbound_state(
    conn: &Connection,
    package_id: &str,
    state: InboundState,
    last_error: Option<&str>,
) -> Result<()> {
    if state.is_terminal() {
        conn.execute(
            "UPDATE sync_inbound SET state = ?1, last_error = ?2, finished_at = ?3 WHERE package_id = ?4",
            params![state.as_str(), last_error, now_iso(), package_id],
        )
    } else {
        conn.execute(
            "UPDATE sync_inbound SET state = ?1, last_error = ?2 WHERE package_id = ?3",
            params![state.as_str(), last_error, package_id],
        )
    }
    .with_context(|| format!("set sync_inbound state {} for {package_id}", state.as_str()))?;
    Ok(())
}

/// Record the cumulative bytes fetched so far for `package_id`. Called at the
/// transport's batch-progress cadence during the fetch stage; best-effort from
/// the sink's side (a failed write warns and never aborts the fetch). Keyed by
/// `package_id` alone — see the uniqueness note on [`set_inbound_state`].
pub fn set_inbound_bytes_done(conn: &Connection, package_id: &str, bytes_done: u64) -> Result<()> {
    conn.execute(
        "UPDATE sync_inbound SET bytes_done = ?1 WHERE package_id = ?2",
        params![bytes_done as i64, package_id],
    )
    .with_context(|| format!("set sync_inbound bytes_done for {package_id}"))?;
    Ok(())
}

/// Every non-terminal inbound row (state NOT IN `done`/`failed`/`cancelled`),
/// oldest-first — the receive-side crash-resume / Active-tab enumeration.
pub fn inbound_active(conn: &Connection) -> Result<Vec<InboundRow>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {INBOUND_COLS} FROM sync_inbound
             WHERE state NOT IN ('done', 'failed', 'cancelled')
             ORDER BY id ASC"
        ))
        .context("prepare inbound_active")?;
    let raws = stmt
        .query_map([], inbound_raw_from_row)
        .context("query inbound_active")?
        .collect::<rusqlite::Result<Vec<InboundRaw>>>()
        .context("collect inbound_active")?;
    raws.into_iter().map(to_inbound).collect()
}

/// Load `sync_inbound` rows in a TERMINAL state (`done`/`failed`/`cancelled`),
/// newest-first, capped at `limit` — the receive-side mirror of
/// [`terminal_outbound`]. [`inbound_active`] returns only the in-flight rows, so
/// a finished inbound batch's durable row id (needed for its Files/Log detail)
/// would be lost from the Transfers list after a restart; this resurrects the
/// recent terminal window from the durable table. Ordered by `finished_at` where
/// present, else `created_at`, then `id DESC`.
pub fn terminal_inbound(conn: &Connection, limit: u32) -> Result<Vec<InboundRow>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {INBOUND_COLS} FROM sync_inbound
             WHERE state IN ('done', 'failed', 'cancelled')
             ORDER BY COALESCE(finished_at, created_at) DESC, id DESC
             LIMIT ?1"
        ))
        .context("prepare terminal_inbound")?;
    let raws = stmt
        .query_map(params![limit], inbound_raw_from_row)
        .context("query terminal_inbound")?
        .collect::<rusqlite::Result<Vec<InboundRaw>>>()
        .context("collect terminal_inbound")?;
    raws.into_iter().map(to_inbound).collect()
}

/// Fetch a single `sync_inbound` row by `package_id` (`None` if absent). Keyed
/// by `package_id` alone — see the uniqueness note on [`set_inbound_state`].
pub fn get_inbound(conn: &Connection, package_id: &str) -> Result<Option<InboundRow>> {
    let raw = conn
        .query_row(
            &format!("SELECT {INBOUND_COLS} FROM sync_inbound WHERE package_id = ?1"),
            params![package_id],
            inbound_raw_from_row,
        )
        .optional()
        .context("query sync_inbound by package_id")?;
    raw.map(to_inbound).transpose()
}

/// Fetch a single `sync_inbound` row by its durable row `id` (`None` if absent).
/// The by-id read behind [`list_transfer_files`](crate::api::sync::list_transfer_files)'s
/// received-detail path (Task 14): the Transfers UI holds the row id from the
/// [`InboundSummary`](crate::sync::status::InboundSummary), not the wire package
/// id, so the detail read resolves the row (and thence its `package_id`) by id.
pub fn get_inbound_by_row_id(conn: &Connection, id: i64) -> Result<Option<InboundRow>> {
    let raw = conn
        .query_row(
            &format!("SELECT {INBOUND_COLS} FROM sync_inbound WHERE id = ?1"),
            params![id],
            inbound_raw_from_row,
        )
        .optional()
        .context("query sync_inbound by id")?;
    raw.map(to_inbound).transpose()
}

/// Fetch the inbound row for `(peer_hex, batch_uuid)` (`None` if absent) — the
/// batch-model lookup the receiver uses to correlate a v3 announce / revoke to its
/// long-lived row (Transfers Batch Model, §D1). Anchored by the
/// `idx_sync_inbound_batch` unique index.
pub fn get_inbound_by_batch(
    conn: &Connection,
    peer_hex: &str,
    batch_uuid: &str,
) -> Result<Option<InboundRow>> {
    let raw = conn
        .query_row(
            &format!("SELECT {INBOUND_COLS} FROM sync_inbound WHERE peer = ?1 AND batch_uuid = ?2"),
            params![peer_hex, batch_uuid],
            inbound_raw_from_row,
        )
        .optional()
        .context("query sync_inbound by (peer, batch_uuid)")?;
    raw.map(to_inbound).transpose()
}

/// Upsert the inbound row for `(peer_hex, batch_uuid)` at the start of a receive
/// attempt (Transfers Batch Model, §D1) — the batch-keyed successor to
/// [`upsert_inbound_announced`]. Keyed on the durable `batch_uuid` (not the
/// per-attempt wire id), so every attempt of one transfer resolves to ONE
/// long-lived row.
///
/// - **No row** for `(peer, batch_uuid)` → INSERT a fresh `announced` row (bytes 0),
///   stamping this attempt's `wire_package_id` and the `batch_uuid`.
/// - **Existing row** → reset it to `announced` with the new `wire_package_id`,
///   `bytes_done 0`, `last_error` / `finished_at` cleared, `frame_count` /
///   `byte_size` refreshed, and `generation += 1` (the user-facing "attempt N"
///   counter — §D5). `display_name` and `landing_dir` are PRESERVED — a
///   resend keeps its name and lands into the same tree.
/// - **Declined is final** (Decline Finality Axis §D3): a row with a non-NULL
///   `declined_at` is left COMPLETELY untouched and returned with
///   `declined_final = true`, so the receiver re-acks the decline rather than
///   re-fetching — regardless of what the attempt-level `state` says by now (a
///   restart reconcile may have overwritten it to `failed`). A sender-revoked
///   `cancelled` row has `declined_at` NULL and resets like any other attempt
///   terminal, so a sender's resend after its own cancel works. Every other case
///   returns `declined_final = false`.
///
/// Returns `(id, declined_final)`. The `SELECT`-then-write is safe because the
/// receiver holds the only connection that writes inbound rows and processes each
/// PEER's announces serially on that peer's lane (W2) — the row key is
/// `(peer, batch_uuid)`, so per-peer FIFO is exactly the single-writer guarantee
/// this needs; two different peers can only ever touch two different rows (same
/// assumption as [`upsert_inbound_announced`]).
pub fn upsert_inbound_attempt(
    conn: &Connection,
    peer_hex: &str,
    batch_uuid: &str,
    wire_package_id: &str,
    frame_count: u32,
    byte_size: u64,
) -> Result<(i64, bool)> {
    let existing: Option<(i64, Option<String>)> = conn
        .query_row(
            "SELECT id, declined_at FROM sync_inbound WHERE peer = ?1 AND batch_uuid = ?2",
            params![peer_hex, batch_uuid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .context("look up sync_inbound by (peer, batch_uuid)")?;
    match existing {
        // Declined-is-final: leave the row untouched, signal the caller.
        Some((id, Some(_))) => Ok((id, true)),
        Some((id, None)) => {
            conn.execute(
                "UPDATE sync_inbound
                 SET state = ?1, package_id = ?2, frame_count = ?3, byte_size = ?4,
                     bytes_done = 0, last_error = NULL, finished_at = NULL,
                     generation = generation + 1
                 WHERE id = ?5",
                params![
                    InboundState::Announced.as_str(),
                    wire_package_id,
                    frame_count,
                    byte_size as i64,
                    id,
                ],
            )
            .with_context(|| format!("reset sync_inbound attempt for {id}"))?;
            Ok((id, false))
        }
        None => {
            conn.execute(
                "INSERT INTO sync_inbound
                 (peer, package_id, batch_uuid, state, frame_count, byte_size, bytes_done, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
                params![
                    peer_hex,
                    wire_package_id,
                    batch_uuid,
                    InboundState::Announced.as_str(),
                    frame_count,
                    byte_size as i64,
                    now_iso(),
                ],
            )
            .context("insert sync_inbound attempt")?;
            Ok((conn.last_insert_rowid(), false))
        }
    }
}

// ── Transfers Status Model v2: display_name, per-file rows, event journal ────
//
// Free fns over a `&Connection`, matching the receipt / source / inbound family:
// the receiver holds a concrete `CatalogSyncStore` and calls these via
// [`CatalogSyncStore::lock_conn`]; later sender/api tasks reach them the same way.
// The SQL is the single source shared by both store impls, so nothing here grows
// the [`SyncStore`] trait (no engine / mock-store churn this task).

/// Persist (or clear with `None`) the human batch name on an outbound row
/// (Transfers Status Model v2 §D1). The send path (T3) writes the auto-derived
/// name here after enqueue; keyed by row `id`.
pub fn set_outbound_display_name(
    conn: &Connection,
    id: i64,
    display_name: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE sync_outbound SET display_name = ?1 WHERE id = ?2",
        params![display_name, id],
    )
    .with_context(|| format!("set display_name for outbound {id}"))?;
    Ok(())
}

/// Stamp the transfer-level receiver-decline marker on an inbound row (Decline
/// Finality Axis §D2). First-write-wins (`COALESCE`): re-stamping an already
/// declined row is a no-op, so the earliest decline instant is preserved across
/// the primary write (`cancel_incoming_package`) and the two repair sites
/// (cancel epilogue / all-cancelled replay). Keyed by row `id` — deliberately NOT
/// the `state` column, so this is safe to write while a fetch is live (no race
/// with the fetch loop's state writes) and survives any later `state` overwrite
/// (sender revoke, restart reconcile).
pub fn set_inbound_declined_at(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE sync_inbound SET declined_at = COALESCE(declined_at, ?1) WHERE id = ?2",
        params![now_iso(), id],
    )
    .with_context(|| format!("set declined_at for inbound {id}"))?;
    Ok(())
}

/// Rotate a declined row's `package_id` to the wire id of the attempt that holds
/// (or is about to hold) its authoritative full `Cancelled` receipt set (Decline
/// Finality Axis §D5). Invariant: a declined row's `package_id` always names the
/// receipt-anchor wire id, so the resend seeding (`load_receipts(row.package_id)`)
/// finds the newest set instead of orphaning it — and every downstream
/// `package_id`-keyed write in the epilogue matches the row again. Keyed by row
/// `id`; rotating to the already-current wire id is a no-op-shaped write.
pub fn rotate_inbound_package_id(conn: &Connection, id: i64, wire_package_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE sync_inbound SET package_id = ?1 WHERE id = ?2",
        params![wire_package_id, id],
    )
    .with_context(|| format!("rotate package_id for inbound {id}"))?;
    Ok(())
}

/// [`set_inbound_declined_at`] + [`rotate_inbound_package_id`] as ONE statement —
/// the cancel epilogue's entry write (Decline Finality Axis §D2 repair 1 + §D5),
/// merged so the serial receiver loop pays one commit instead of two while
/// holding the store lock.
pub fn mark_inbound_declined_with_anchor(
    conn: &Connection,
    id: i64,
    wire_package_id: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE sync_inbound
         SET declined_at = COALESCE(declined_at, ?1), package_id = ?2
         WHERE id = ?3",
        params![now_iso(), wire_package_id, id],
    )
    .with_context(|| format!("mark inbound {id} declined with anchor"))?;
    Ok(())
}

/// Persist (or clear with `None`) the human batch name on an inbound row
/// (Transfers Status Model v2 §D1). The receiver (T5) writes the v2 announce's
/// `batch_name` here; keyed by row `id`.
pub fn set_inbound_display_name(
    conn: &Connection,
    id: i64,
    display_name: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE sync_inbound SET display_name = ?1 WHERE id = ?2",
        params![display_name, id],
    )
    .with_context(|| format!("set display_name for inbound {id}"))?;
    Ok(())
}

/// Stamp the announcing peer's device capability (`"athenaeum"` / `"perseus"`)
/// onto an inbound row (Perseus UI v2, Task 9). The receiver writes this at
/// announce time from the cached device-list capability map so the Transfers UI
/// can show whether a received transfer came from a full Athenaeum peer or a
/// send-only Perseus agent — and, by living on the row, the stamp survives a
/// later device revocation that empties that cache. Keyed by row `id`;
/// best-effort at the call site (a cache miss simply leaves the column `NULL`).
pub fn set_inbound_peer_capability(conn: &Connection, id: i64, cap: &str) -> Result<()> {
    conn.execute(
        "UPDATE sync_inbound SET peer_capability = ?2 WHERE id = ?1",
        params![id, cap],
    )
    .context("set sync_inbound.peer_capability")?;
    Ok(())
}

/// Replace the entire per-file row set for an outbound batch (Transfers Status
/// Model v2 §D4). Deletes any existing `sync_outbound_files` rows for
/// `outbound_id`, then inserts `files` verbatim (each row's own `updated_at` is
/// honoured) — the whole swap is one transaction so a reader never sees a partial
/// set. Called once at package-build time; a resend that rebuilds the manifest
/// re-runs it. Idempotent by the swap.
pub fn replace_outbound_files(
    conn: &Connection,
    outbound_id: i64,
    files: &[OutboundFileRow],
) -> Result<()> {
    let tx = conn
        .unchecked_transaction()
        .context("begin replace_outbound_files")?;
    tx.execute(
        "DELETE FROM sync_outbound_files WHERE outbound_id = ?1",
        params![outbound_id],
    )
    .context("clear sync_outbound_files")?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO sync_outbound_files
                 (outbound_id, rel_path, byte_size, frame_uuid, state, bytes_done, outcome, error, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )
            .context("prepare insert sync_outbound_files")?;
        for f in files {
            stmt.execute(params![
                outbound_id,
                f.rel_path,
                f.byte_size as i64,
                f.frame_uuid,
                f.state.as_str(),
                f.bytes_done as i64,
                f.outcome,
                f.error,
                f.updated_at,
            ])
            .with_context(|| format!("insert sync_outbound_files row {}", f.rel_path))?;
        }
    }
    tx.commit().context("commit replace_outbound_files")?;
    Ok(())
}

/// Replace the entire per-file row set for an inbound batch (Transfers Status
/// Model v2 §D4). The receive-side twin of [`replace_outbound_files`]; called
/// once at announce time from the v2 manifest.
pub fn replace_inbound_files(
    conn: &Connection,
    inbound_id: i64,
    files: &[InboundFileRow],
) -> Result<()> {
    let tx = conn
        .unchecked_transaction()
        .context("begin replace_inbound_files")?;
    tx.execute(
        "DELETE FROM sync_inbound_files WHERE inbound_id = ?1",
        params![inbound_id],
    )
    .context("clear sync_inbound_files")?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO sync_inbound_files
                 (inbound_id, rel_path, byte_size, frame_uuid, state, bytes_done, outcome, error, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )
            .context("prepare insert sync_inbound_files")?;
        for f in files {
            stmt.execute(params![
                inbound_id,
                f.rel_path,
                f.byte_size as i64,
                f.frame_uuid,
                f.state.as_str(),
                f.bytes_done as i64,
                f.outcome,
                f.error,
                f.updated_at,
            ])
            .with_context(|| format!("insert sync_inbound_files row {}", f.rel_path))?;
        }
    }
    tx.commit().context("commit replace_inbound_files")?;
    Ok(())
}

/// Update one outbound file's mutable fields — `state`, `bytes_done`, `outcome`,
/// `error` — stamping a fresh `updated_at` (Transfers Status Model v2 §D4). Keyed
/// on `(outbound_id, rel_path)`; the row is created up front by
/// [`replace_outbound_files`], so this is an in-place UPDATE (a no-op if the file
/// is unknown). Called on every per-file transition and serve-progress checkpoint.
pub fn set_outbound_file_state(
    conn: &Connection,
    outbound_id: i64,
    rel_path: &str,
    state: OutboundFileState,
    bytes_done: u64,
    outcome: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    let changed = conn
        .execute(
            "UPDATE sync_outbound_files
             SET state = ?1, bytes_done = ?2, outcome = ?3, error = ?4, updated_at = ?5
             WHERE outbound_id = ?6 AND rel_path = ?7",
            params![
                state.as_str(),
                bytes_done as i64,
                outcome,
                error,
                now_iso(),
                outbound_id,
                rel_path
            ],
        )
        .with_context(|| format!("set outbound file state for {outbound_id}/{rel_path}"))?;
    if changed == 0 {
        // An unknown rel_path (e.g. a served `manifest.ndjson`, or a Perseus batch
        // enqueued with no per-file rows). Not an error — the setter is a no-op
        // upsert-in-place; log at debug so a genuinely missing row is discoverable.
        tracing::debug!(
            outbound_id,
            rel_path,
            state = state.as_str(),
            "set_outbound_file_state matched 0 rows"
        );
    }
    Ok(())
}

/// Update one inbound file's mutable fields (Transfers Status Model v2 §D4). The
/// receive-side twin of [`set_outbound_file_state`]; keyed on `(inbound_id,
/// rel_path)`, in-place UPDATE over a row created by [`replace_inbound_files`].
pub fn set_inbound_file_state(
    conn: &Connection,
    inbound_id: i64,
    rel_path: &str,
    state: InboundFileState,
    bytes_done: u64,
    outcome: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    let changed = conn
        .execute(
            "UPDATE sync_inbound_files
             SET state = ?1, bytes_done = ?2, outcome = ?3, error = ?4, updated_at = ?5
             WHERE inbound_id = ?6 AND rel_path = ?7",
            params![
                state.as_str(),
                bytes_done as i64,
                outcome,
                error,
                now_iso(),
                inbound_id,
                rel_path
            ],
        )
        .with_context(|| format!("set inbound file state for {inbound_id}/{rel_path}"))?;
    if changed == 0 {
        // Same discoverability guard as the outbound twin: a v1/manifest-less batch
        // has no rows, so the no-op is legitimate — but a missing row on a v2 batch
        // must be findable in the logs, never silent.
        tracing::debug!(
            inbound_id,
            rel_path,
            state = state.as_str(),
            "set_inbound_file_state matched 0 rows"
        );
    }
    Ok(())
}

/// Persist the resolved on-disk landing directory for an inbound package
/// (Transfers Status Model v2 §D2). Written ONCE per package at first landing so a
/// resume/restart lands into the same tree; keyed by row `id`. See
/// [`InboundRow::landing_dir`](super::models::InboundRow::landing_dir).
pub fn set_inbound_landing_dir(conn: &Connection, id: i64, landing_dir: &str) -> Result<()> {
    conn.execute(
        "UPDATE sync_inbound SET landing_dir = ?1 WHERE id = ?2",
        params![landing_dir, id],
    )
    .with_context(|| format!("set landing_dir for inbound {id}"))?;
    Ok(())
}

/// True if some OTHER (id ≠ `exclude_id`) NON-terminal inbound row already claims
/// `landing_dir` (Transfers Status Model v2 §D2 collision rule). A terminal prior
/// batch (done/failed/cancelled) with the same dir does NOT count — repeat sends
/// of the same object merge into one tree — so this only reports a live conflict
/// that must be suffixed `_2`/`_3`…
pub fn landing_dir_claimed_by_active(
    conn: &Connection,
    landing_dir: &str,
    exclude_id: i64,
) -> Result<bool> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_inbound
             WHERE landing_dir = ?1 AND id <> ?2
               AND state NOT IN ('done', 'failed', 'cancelled')",
            params![landing_dir, exclude_id],
            |r| r.get(0),
        )
        .context("count active landing_dir claims")?;
    Ok(n > 0)
}

/// Settle every not-yet-`done` per-file row of an inbound batch to `state` with
/// `outcome`/`error` in ONE UPDATE (Transfers Status Model v2 §D4). The batch-level
/// closer used on a fetch/ingest failure (→ `failed` + reason), a receiver cancel
/// (→ `done` + `cancelled`), and the restart reconcile (→ `failed` + "interrupted
/// by restart"). Rows already `done` (per-frame verdict recorded) are left intact.
pub fn settle_unsettled_inbound_files(
    conn: &Connection,
    inbound_id: i64,
    state: InboundFileState,
    outcome: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE sync_inbound_files
         SET state = ?1, outcome = ?2, error = ?3, updated_at = ?4
         WHERE inbound_id = ?5 AND state <> 'done'",
        params![state.as_str(), outcome, error, now_iso(), inbound_id],
    )
    .with_context(|| format!("settle unsettled inbound files for {inbound_id}"))?;
    Ok(())
}

/// Raw column tuple for a `sync_outbound_files` / `sync_inbound_files` row (both
/// tables share the same nine-column shape); parsed by [`to_outbound_file`] /
/// [`to_inbound_file`] so fallible state parsing stays outside the rusqlite closure.
type FileRaw = (
    i64,
    String,
    i64,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    String,
);

fn file_raw_from_row(r: &rusqlite::Row) -> rusqlite::Result<FileRaw> {
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
}

fn to_outbound_file(raw: FileRaw) -> Result<OutboundFileRow> {
    let (
        outbound_id,
        rel_path,
        byte_size,
        frame_uuid,
        state,
        bytes_done,
        outcome,
        error,
        updated_at,
    ) = raw;
    Ok(OutboundFileRow {
        outbound_id,
        rel_path,
        byte_size: byte_size.max(0) as u64,
        frame_uuid,
        state: OutboundFileState::from_db(&state)?,
        bytes_done: bytes_done.max(0) as u64,
        outcome,
        error,
        updated_at,
    })
}

fn to_inbound_file(raw: FileRaw) -> Result<InboundFileRow> {
    let (
        inbound_id,
        rel_path,
        byte_size,
        frame_uuid,
        state,
        bytes_done,
        outcome,
        error,
        updated_at,
    ) = raw;
    Ok(InboundFileRow {
        inbound_id,
        rel_path,
        byte_size: byte_size.max(0) as u64,
        frame_uuid,
        state: InboundFileState::from_db(&state)?,
        bytes_done: bytes_done.max(0) as u64,
        outcome,
        error,
        updated_at,
    })
}

/// Every per-file row for an outbound batch, ordered by `rel_path` for a
/// deterministic tree (Transfers Status Model v2 §D4).
pub fn list_outbound_files(conn: &Connection, outbound_id: i64) -> Result<Vec<OutboundFileRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT outbound_id, rel_path, byte_size, frame_uuid, state, bytes_done, outcome, error, updated_at
             FROM sync_outbound_files WHERE outbound_id = ?1 ORDER BY rel_path ASC",
        )
        .context("prepare list_outbound_files")?;
    let raws = stmt
        .query_map(params![outbound_id], file_raw_from_row)
        .context("query list_outbound_files")?
        .collect::<rusqlite::Result<Vec<FileRaw>>>()
        .context("collect list_outbound_files")?;
    raws.into_iter().map(to_outbound_file).collect()
}

/// Every per-file row for an inbound batch, ordered by `rel_path` (Transfers
/// Status Model v2 §D4).
pub fn list_inbound_files(conn: &Connection, inbound_id: i64) -> Result<Vec<InboundFileRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT inbound_id, rel_path, byte_size, frame_uuid, state, bytes_done, outcome, error, updated_at
             FROM sync_inbound_files WHERE inbound_id = ?1 ORDER BY rel_path ASC",
        )
        .context("prepare list_inbound_files")?;
    let raws = stmt
        .query_map(params![inbound_id], file_raw_from_row)
        .context("query list_inbound_files")?
        .collect::<rusqlite::Result<Vec<FileRaw>>>()
        .context("collect list_inbound_files")?;
    raws.into_iter().map(to_inbound_file).collect()
}

/// Grouped per-file rollup for a set of outbound batch ids — ONE query for the
/// whole set (never a per-row loop), for the cheap status poll (Transfers Status
/// Model v2 §D5). Only ids with ≥1 per-file row appear in the map; a legacy batch
/// with no per-file rows is simply absent (the caller defaults it to zeroes).
///
/// `done`/`failed` are mutually exclusive:
/// - **failed** — `state = 'failed'` OR `outcome` starts with `rejected`.
/// - **done** — (`state = 'done'` OR `state = 'uploaded'`) AND not failed; a
///   user-cancelled file (`outcome = 'cancelled'`, `state = 'done'`) therefore
///   counts as done — terminal, not an error.
pub fn outbound_file_counts(
    conn: &Connection,
    ids: &[i64],
) -> Result<HashMap<i64, TransferFileCounts>> {
    grouped_file_counts(conn, "sync_outbound_files", "outbound_id", ids)
}

/// The receive-side twin of [`outbound_file_counts`] — ONE grouped query over
/// `sync_inbound_files`.
///
/// Both directions share the same "done" predicate because they share the same
/// rung: the sender's `uploaded` and the receiver's `fetched` both mean "bytes
/// complete, verdict pending" (D2 §3.4), so both counters climb during a transfer
/// for the same reason and through the same statement. `state = 'uploaded'` never
/// occurs inbound and `state = 'fetched'` never occurs outbound; the shared CASE
/// simply admits both.
pub fn inbound_file_counts(
    conn: &Connection,
    ids: &[i64],
) -> Result<HashMap<i64, TransferFileCounts>> {
    grouped_file_counts(conn, "sync_inbound_files", "inbound_id", ids)
}

/// Shared implementation for [`outbound_file_counts`] / [`inbound_file_counts`]:
/// one `GROUP BY` over the parent-id `IN (…)` set. `COALESCE(outcome,'')` keeps
/// the `rejected%` predicate boolean (a NULL `outcome` must not poison the
/// `done` CASE via three-valued logic), so an `uploaded`/`fetched` file with no
/// outcome still counts as done — that pair is the same rung on the two sides
/// (D2 §3.4), which is why one CASE serves both directions.
///
/// The two trailing `duplicate` columns (count + `byte_size` sum over
/// `outcome = 'duplicate'` rows) ride the same statement: they are a subset of
/// `done` that the send-side progress line subtracts (see
/// [`TransferFileCounts`]), so they must come from the same snapshot as `done`
/// or the subtraction could go negative mid-settle.
fn grouped_file_counts(
    conn: &Connection,
    table: &str,
    id_col: &str,
    ids: &[i64],
) -> Result<HashMap<i64, TransferFileCounts>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = (1..=ids.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT {id_col}, \
                COUNT(*), \
                SUM(CASE WHEN state = 'failed' OR COALESCE(outcome,'') LIKE 'rejected%' \
                         THEN 1 ELSE 0 END), \
                SUM(CASE WHEN (state = 'done' OR state = 'uploaded' OR state = 'fetched') \
                          AND NOT (state = 'failed' OR COALESCE(outcome,'') LIKE 'rejected%') \
                         THEN 1 ELSE 0 END), \
                SUM(CASE WHEN COALESCE(outcome,'') = 'duplicate' THEN 1 ELSE 0 END), \
                SUM(CASE WHEN COALESCE(outcome,'') = 'duplicate' THEN byte_size ELSE 0 END) \
         FROM {table} WHERE {id_col} IN ({placeholders}) GROUP BY {id_col}"
    );
    let mut stmt = conn.prepare(&sql).context("prepare grouped_file_counts")?;
    let rows = stmt
        .query_map(params_from_iter(ids.iter()), |r| {
            let id: i64 = r.get(0)?;
            let total: i64 = r.get(1)?;
            let failed: i64 = r.get(2)?;
            let done: i64 = r.get(3)?;
            let duplicate: i64 = r.get(4)?;
            let duplicate_bytes: i64 = r.get(5)?;
            Ok((id, total, done, failed, duplicate, duplicate_bytes))
        })
        .context("query grouped_file_counts")?;
    let mut map = HashMap::new();
    for row in rows {
        let (id, total, done, failed, duplicate, duplicate_bytes) =
            row.context("row grouped_file_counts")?;
        map.insert(
            id,
            TransferFileCounts {
                total: total.max(0) as u32,
                done: done.max(0) as u32,
                failed: failed.max(0) as u32,
                duplicate: duplicate.max(0) as u32,
                duplicate_bytes: duplicate_bytes.max(0) as u64,
            },
        );
    }
    Ok(map)
}

/// Delete every per-file row for an outbound batch — the delete-with-parent
/// cascade for `sync_outbound_files` (Transfers Status Model v2 §D4). This store
/// uses no FK cascades, so a future outbound-row delete path must call this (plus
/// [`delete_sync_events`] for the `Sent` journal) to avoid orphaned rows. No such
/// delete path exists in the store today (retention deletes source files, never
/// the outbound rows), so this is currently unwired but ready.
pub fn delete_outbound_files(conn: &Connection, outbound_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM sync_outbound_files WHERE outbound_id = ?1",
        params![outbound_id],
    )
    .with_context(|| format!("delete sync_outbound_files for {outbound_id}"))?;
    Ok(())
}

/// Delete every per-file row for an inbound batch — the delete-with-parent
/// cascade for `sync_inbound_files` (see [`delete_outbound_files`]). Pair with
/// [`delete_sync_events`] for the `Received` journal when an inbound-row delete
/// path is added.
pub fn delete_inbound_files(conn: &Connection, inbound_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM sync_inbound_files WHERE inbound_id = ?1",
        params![inbound_id],
    )
    .with_context(|| format!("delete sync_inbound_files for {inbound_id}"))?;
    Ok(())
}

/// Newest-per-batch cap for [`append_sync_event`]: each insert prunes the batch's
/// journal down to this many rows (Transfers Status Model v2 §D7).
pub const SYNC_EVENTS_CAP: u32 = 200;

/// Append one event to a batch's journal, then prune the journal to the newest
/// [`SYNC_EVENTS_CAP`] rows for that `(direction, batch_key)` — insert + prune in
/// one transaction (Transfers Status Model v2 §D7). `batch_key` is the outbound
/// row id (sent) or inbound row id (received) as text; `ts` is stamped `now`.
pub fn append_sync_event(
    conn: &Connection,
    direction: Direction,
    batch_key: &str,
    kind: &str,
    detail: Option<&str>,
) -> Result<()> {
    let dir = direction.as_str();
    let tx = conn
        .unchecked_transaction()
        .context("begin append_sync_event")?;
    tx.execute(
        "INSERT INTO sync_events (direction, batch_key, ts, kind, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![dir, batch_key, now_iso(), kind, detail],
    )
    .context("insert sync_events")?;
    // Keep only the newest SYNC_EVENTS_CAP rows for this batch's journal.
    tx.execute(
        "DELETE FROM sync_events
         WHERE direction = ?1 AND batch_key = ?2
           AND id NOT IN (
               SELECT id FROM sync_events
               WHERE direction = ?1 AND batch_key = ?2
               ORDER BY id DESC LIMIT ?3
           )",
        params![dir, batch_key, SYNC_EVENTS_CAP],
    )
    .context("prune sync_events")?;
    tx.commit().context("commit append_sync_event")?;
    Ok(())
}

/// Every event in a batch's journal, newest-first (Transfers Status Model v2 §D7 —
/// the detail pane's **Log** tab). Scoped to one `(direction, batch_key)` pair.
pub fn list_sync_events(
    conn: &Connection,
    direction: Direction,
    batch_key: &str,
) -> Result<Vec<SyncEventRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, direction, ts, kind, detail FROM sync_events
             WHERE direction = ?1 AND batch_key = ?2 ORDER BY id DESC",
        )
        .context("prepare list_sync_events")?;
    let rows = stmt
        .query_map(params![direction.as_str(), batch_key], |r| {
            let id: i64 = r.get(0)?;
            let dir: String = r.get(1)?;
            let ts: String = r.get(2)?;
            let kind: String = r.get(3)?;
            let detail: Option<String> = r.get(4)?;
            Ok((id, dir, ts, kind, detail))
        })
        .context("query list_sync_events")?
        .collect::<rusqlite::Result<Vec<(i64, String, String, String, Option<String>)>>>()
        .context("collect list_sync_events")?;
    rows.into_iter()
        .map(|(id, dir, ts, kind, detail)| {
            Ok(SyncEventRow {
                id,
                direction: Direction::from_db(&dir)?,
                ts,
                kind,
                detail,
            })
        })
        .collect()
}

/// Delete a batch's entire event journal — the delete-with-parent cascade for
/// `sync_events` (Transfers Status Model v2 §D7). Pair with
/// [`delete_outbound_files`] / [`delete_inbound_files`] when a parent-row delete
/// path is added; unwired today for the same reason (no outbound/inbound row
/// delete path exists in the store).
pub fn delete_sync_events(conn: &Connection, direction: Direction, batch_key: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM sync_events WHERE direction = ?1 AND batch_key = ?2",
        params![direction.as_str(), batch_key],
    )
    .with_context(|| format!("delete sync_events for {}/{batch_key}", direction.as_str()))?;
    Ok(())
}

// ── batch record deletion (Transfers Status Model v2, UX wave 2) ─────────────
//
// The parent-row deletes + lean scan reads that back
// [`delete_transfer_history`](crate::api::sync::delete_transfer_history) — the
// Transfers UI's "remove a batch from history" action. This store uses no FK
// cascades, so the api layer composes these with the per-file
// ([`delete_outbound_files`] / [`delete_inbound_files`]) + event
// ([`delete_sync_events`]) cascades under ONE transaction.

/// Every `sync_outbound` row as a lightweight `(id, package_ref, state)` tuple.
/// The sent-batch delete scan reads these and compares each `package_ref`
/// basename in Rust to group a batch's attempts (the key is a dir basename, which
/// no `LIKE` predicate can match safely). Deliberately lean — it does NOT parse
/// the `peer` hex like [`all_outbound_rows`], so one malformed legacy row can
/// never abort a delete.
pub fn outbound_ref_states(conn: &Connection) -> Result<Vec<(i64, String, String)>> {
    let mut stmt = conn
        .prepare("SELECT id, package_ref, state FROM sync_outbound")
        .context("prepare outbound_ref_states")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .context("query outbound_ref_states")?
        .collect::<rusqlite::Result<Vec<(i64, String, String)>>>()
        .context("collect outbound_ref_states")?;
    Ok(rows)
}

/// The raw `peer` hex of one `sync_outbound` row, or `None` if the row is absent.
/// Lean sibling of [`outbound_ref_states`] for the [`delete_transfer_history`]
/// dead-peer split: returns the stored hex string verbatim (NO `NodeId` parse) so
/// it compares directly against the cached `SYNC_AUTHORIZED_PEERS` hex allow-list
/// and a malformed legacy hex can never abort a delete.
///
/// [`delete_transfer_history`]: crate::api::sync::delete_transfer_history
pub fn outbound_peer_hex(conn: &Connection, id: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT peer FROM sync_outbound WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )
    .optional()
    .with_context(|| format!("read peer hex for outbound {id}"))
}

/// Force one `sync_outbound` row to terminal `cancelled` on an arbitrary
/// connection/transaction — the free-function form of [`SyncStore::set_state`]`(id,
/// Cancelled)` for the [`delete_transfer_history`] dead-peer path, which runs
/// inside its own `BEGIN IMMEDIATE` tx and cannot borrow the engine's locked
/// store. Clears `next_retry_at` (a terminal state has no pending retry), matching
/// the trait impl's terminal-state write.
///
/// [`delete_transfer_history`]: crate::api::sync::delete_transfer_history
pub fn cancel_outbound_row(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE sync_outbound SET state = ?1, next_retry_at = NULL WHERE id = ?2",
        params![OutboundState::Cancelled.as_str(), id],
    )
    .with_context(|| format!("cancel outbound row {id}"))?;
    Ok(())
}

/// Settle every not-yet-`done` per-file row of an OUTBOUND batch to terminal
/// `done` carrying `outcome` in ONE UPDATE — the outbound twin of
/// [`settle_unsettled_inbound_files`], matching the engine's per-row terminal
/// settle (`state=done`, `outcome=<outcome>`; see `settle_unsettled_files` in
/// `engine.rs`). Rows already `done` are left intact.
pub fn settle_unsettled_outbound_files(
    conn: &Connection,
    outbound_id: i64,
    outcome: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE sync_outbound_files
         SET state = ?1, outcome = ?2, error = NULL, updated_at = ?3
         WHERE outbound_id = ?4 AND state <> 'done'",
        params![
            OutboundFileState::Done.as_str(),
            outcome,
            now_iso(),
            outbound_id
        ],
    )
    .with_context(|| format!("settle unsettled outbound files for {outbound_id}"))?;
    Ok(())
}

/// Every `sync_inbound` row matching `package_id` as a `(id, state)` tuple — the
/// received-half batch-delete scan. A wire package uuid is effectively unique, so
/// this is normally one row, but the `UNIQUE(peer, package_id)` schema permits
/// several across peers, so the delete handles them all.
pub fn inbound_id_states_by_package(
    conn: &Connection,
    package_id: &str,
) -> Result<Vec<(i64, String)>> {
    let mut stmt = conn
        .prepare("SELECT id, state FROM sync_inbound WHERE package_id = ?1")
        .context("prepare inbound_id_states_by_package")?;
    let rows = stmt
        .query_map(params![package_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .context("query inbound_id_states_by_package")?
        .collect::<rusqlite::Result<Vec<(i64, String)>>>()
        .context("collect inbound_id_states_by_package")?;
    Ok(rows)
}

/// Every `sync_inbound` row for one batch as `(id, state, wire package_id)` — the
/// received-half batch-delete scan keyed on the durable `batch_uuid` (Transfers
/// Batch Model, B5b), the symmetric twin of the sent side keying on the package-dir
/// basename. Matches `batch_uuid = key`, with a NULL-edge fallback to
/// `package_id = key` so a legacy row whose `batch_uuid` column is NULL (a v1/v2
/// receive, or a pre-column row) — whose caller passes the wire id — is still
/// deletable. Returns the CURRENT wire `package_id` per matched row too, so the
/// caller can release its blob tags and sweep any history rows written wire-id-keyed
/// before B5b (post-wipe dev-cycle hygiene). Normally one row (a `batch_uuid` is
/// effectively unique), but the `UNIQUE(peer, batch_uuid)` schema permits several
/// across peers, so the delete handles them all.
pub fn inbound_id_states_by_batch(
    conn: &Connection,
    key: &str,
) -> Result<Vec<(i64, String, String)>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, state, package_id FROM sync_inbound \
             WHERE batch_uuid = ?1 OR (batch_uuid IS NULL AND package_id = ?1)",
        )
        .context("prepare inbound_id_states_by_batch")?;
    let rows = stmt
        .query_map(params![key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .context("query inbound_id_states_by_batch")?
        .collect::<rusqlite::Result<Vec<(i64, String, String)>>>()
        .context("collect inbound_id_states_by_batch")?;
    Ok(rows)
}

/// Delete one `sync_outbound` row by id — the parent-row delete of the batch
/// record cascade. Pair with [`delete_outbound_files`] + [`delete_sync_events`]
/// under one transaction (this store has no FK cascades).
pub fn delete_outbound_row(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM sync_outbound WHERE id = ?1", params![id])
        .with_context(|| format!("delete sync_outbound row {id}"))?;
    Ok(())
}

/// Delete one `sync_inbound` row by id — the received-half twin of
/// [`delete_outbound_row`]. Pair with [`delete_inbound_files`] +
/// [`delete_sync_events`] under one transaction.
pub fn delete_inbound_row(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM sync_inbound WHERE id = ?1", params![id])
        .with_context(|| format!("delete sync_inbound row {id}"))?;
    Ok(())
}

/// Delete every `sync_history` row for one batch `(direction, package_id)`,
/// returning the number removed — the history side of the batch record delete.
/// `package_id` is the outbound dir basename (sent) or wire package uuid
/// (received) the history writers stamp into that column.
pub fn delete_history_for_package(
    conn: &Connection,
    direction: Direction,
    package_id: &str,
) -> Result<u32> {
    let n = conn
        .execute(
            "DELETE FROM sync_history WHERE direction = ?1 AND package_id = ?2",
            params![direction.as_str(), package_id],
        )
        .with_context(|| {
            format!(
                "delete sync_history for {}/{package_id}",
                direction.as_str()
            )
        })?;
    Ok(n as u32)
}

/// Insert a new `sync_outbound` row in [`Queued`](OutboundState::Queued) AND its
/// `sync_outbound_files` rows (state `pending`, `bytes_done 0`) AND its
/// `display_name` — all in ONE transaction — returning the new row id (Transfers
/// Status Model v2 §D1/§D4).
///
/// This is the race-kill for the enqueue path: the T3 send path used to write the
/// name + file rows *after* `enqueue_package` returned, so the engine's first
/// announce could fire against a nameless / file-less row. Folding the whole write
/// into the enqueue transaction means the row is never visible to a concurrent
/// reader (or the worker's own `start_package`) without its name + file manifest.
/// `files` are the per-payload `(rel_path, byte_size, frame_uuid)` triples
/// ([`AnnounceFileEntry`]); each becomes a `pending` per-file row. An empty
/// `files` (Perseus / a nameless retry) writes just the row + name.
///
/// `layout` is the transfer's landing shape ([`PackageLayout`], mirror-hierarchy
/// T1/T2), stamped in this same transaction so the announce path can never read
/// a row without it. It is decided ONCE, at enqueue: a resend reuses the row and
/// therefore keeps the stamp — re-labelling an attempt mid-flight would move files
/// the receiver already landed.
pub fn insert_outbound_with_files(
    conn: &Connection,
    package_ref: &str,
    peer_hex: &str,
    display_name: Option<&str>,
    files: &[AnnounceFileEntry],
    layout: PackageLayout,
) -> Result<i64> {
    let tx = conn
        .unchecked_transaction()
        .context("begin insert_outbound_with_files")?;
    tx.execute(
        "INSERT INTO sync_outbound
         (package_ref, peer, state, attempts, created_at, display_name, layout)
         VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6)",
        params![
            package_ref,
            peer_hex,
            OutboundState::Queued.as_str(),
            now_iso(),
            display_name,
            layout.as_str(),
        ],
    )
    .context("insert sync_outbound")?;
    let id = tx.last_insert_rowid();
    if !files.is_empty() {
        let now = now_iso();
        let mut stmt = tx
            .prepare(
                "INSERT INTO sync_outbound_files
                 (outbound_id, rel_path, byte_size, frame_uuid, state, bytes_done, outcome, error, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, NULL, NULL, ?6)",
            )
            .context("prepare insert sync_outbound_files (enqueue)")?;
        for f in files {
            stmt.execute(params![
                id,
                f.rel_path,
                f.byte_size as i64,
                f.frame_uuid,
                OutboundFileState::Pending.as_str(),
                now,
            ])
            .with_context(|| format!("insert sync_outbound_files row {} (enqueue)", f.rel_path))?;
        }
    }
    tx.commit().context("commit insert_outbound_with_files")?;
    Ok(id)
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
        let conn =
            Connection::open(path).with_context(|| format!("open sync db {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA busy_timeout = 5000;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .context("configure sync db pragmas")?;
        // Transfers Batch Model reset (spec §Migration): MUST run before any DDL
        // below — the detector inspects table/column presence, and every
        // `CREATE TABLE IF NOT EXISTS` here un-conditionally materialises its
        // table, so checking any later could no longer tell "freshly created by
        // this very call" from "pre-existing with legacy rows" (see
        // `transfer_wipe_needed`'s docs — this is what catches a pre-Task-11
        // beta.1/beta.2 DB that has `sync_outbound`/`sync_history` rows but no
        // `sync_inbound` table at all). A no-op on a fresh or already-migrated DB.
        wipe_transfer_bookkeeping_on_upgrade(&conn)?;
        conn.execute(DDL_OUTBOUND, [])
            .context("create sync_outbound")?;
        ensure_outbound_columns(&conn)?;
        conn.execute(DDL_HISTORY, [])
            .context("create sync_history")?;
        ensure_history_columns(&conn)?;
        conn.execute(DDL_INBOUND, [])
            .context("create sync_inbound")?;
        ensure_inbound_columns(&conn)?;
        conn.execute(DDL_OUTBOUND_FILES, [])
            .context("create sync_outbound_files")?;
        conn.execute(DDL_INBOUND_FILES, [])
            .context("create sync_inbound_files")?;
        conn.execute(DDL_SYNC_EVENTS, [])
            .context("create sync_events")?;
        for idx in DDL_INDEXES {
            conn.execute(idx, []).context("create sync index")?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Fetch a single outbound row by id. Inherent helper retained so existing
    /// concrete-`StandaloneSyncStore` callers (Perseus, tests) need no `SyncStore`
    /// import; the [`SyncStore::get_outbound`] trait method shares the same
    /// [`outbound_row_by_id`] body.
    pub fn get_outbound(&self, id: i64) -> Result<Option<OutboundRow>> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        outbound_row_by_id(&conn, id)
    }

    /// Every outbound row, newest-first, capped at `limit` — the Perseus web
    /// status page's "sent" list. Delegates to [`all_outbound_rows`]. Inherent
    /// (not part of [`SyncStore`]) because only the web layer needs an unfiltered
    /// listing; the engine drives itself off the state-filtered trait methods.
    pub fn all_outbound(&self, limit: u32) -> Result<Vec<OutboundRow>> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        all_outbound_rows(&conn, limit)
    }

    /// Swap the entire per-file row set for one outbound batch. Delegates to
    /// [`replace_outbound_files`]; inherent (not on [`SyncStore`]) because only
    /// the Perseus resend path uses it — a partial payload rebuild rewrites the
    /// manifest to the restored subset, and the per-file rows must follow it or
    /// the next announce would offer files no longer on disk.
    pub fn replace_outbound_files(
        &self,
        outbound_id: i64,
        files: &[OutboundFileRow],
    ) -> Result<()> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        replace_outbound_files(&conn, outbound_id, files)
    }

    /// The sender-side `sync_events` journal of ONE outbound row (the Perseus web
    /// Log tab's read). Scoped to `Direction::Sent` and the row id as text —
    /// the engine's own journaling convention ([`append_sync_event`] stores the
    /// outbound row id in `batch_key`). Newest-first per [`list_sync_events`].
    /// Inherent (not on [`SyncStore`]): only the Perseus web layer reads a single
    /// row's log.
    pub fn list_sync_events_for(&self, outbound_id: i64) -> Result<Vec<SyncEventRow>> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        list_sync_events(&conn, Direction::Sent, &outbound_id.to_string())
    }

    /// History-delete a whole batch group's sender bookkeeping — each row's
    /// per-file rows and `Sent` journal, then the row itself — in ONE
    /// transaction (this store has no FK cascades, so the three deletes are
    /// composed here). All-or-nothing: a failure rolls the whole group back.
    /// Never touches disk payloads or Perseus's `perseus_seen` linkage, so dedup
    /// identity survives a history delete. An empty `ids` slice is a no-op commit;
    /// an id naming no row deletes nothing for that id (idempotent). Inherent (not
    /// on [`SyncStore`]): only the Perseus web batch-history delete calls it.
    pub fn delete_outbound_group(&self, ids: &[i64]) -> Result<()> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        let tx = conn
            .unchecked_transaction()
            .context("begin delete_outbound_group")?;
        for &id in ids {
            delete_outbound_files(&tx, id)?;
            delete_sync_events(&tx, Direction::Sent, &id.to_string())?;
            delete_outbound_row(&tx, id)?;
        }
        tx.commit().context("commit delete_outbound_group")
    }
}

impl SyncStore for StandaloneSyncStore {
    fn enqueue(
        &self,
        package_ref: &str,
        peer: NodeId,
        display_name: Option<&str>,
        files: &[AnnounceFileEntry],
        layout: PackageLayout,
    ) -> Result<i64> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        let peer_hex = node_id_hex(&peer);
        insert_outbound_with_files(&conn, package_ref, &peer_hex, display_name, files, layout)
    }

    fn replace_outbound_files(&self, outbound_id: i64, files: &[OutboundFileRow]) -> Result<()> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        replace_outbound_files(&conn, outbound_id, files)
    }

    fn list_outbound_files(&self, outbound_id: i64) -> Result<Vec<OutboundFileRow>> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        list_outbound_files(&conn, outbound_id)
    }

    fn set_outbound_file_state(
        &self,
        outbound_id: i64,
        rel_path: &str,
        state: OutboundFileState,
        bytes_done: u64,
        outcome: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        set_outbound_file_state(
            &conn,
            outbound_id,
            rel_path,
            state,
            bytes_done,
            outcome,
            error,
        )
    }

    fn append_sync_event(
        &self,
        direction: Direction,
        batch_key: &str,
        kind: &str,
        detail: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        append_sync_event(&conn, direction, batch_key, kind, detail)
    }

    fn set_state(&self, id: i64, s: OutboundState) -> Result<()> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        // Defense-in-depth (Task 2 M1): a terminal state has no pending retry,
        // so clear any persisted countdown in the same write — a future
        // non-engine caller can't leave a stale `next_retry_at` behind. The
        // engine also clears it explicitly; that duplicate is harmless.
        if s.is_terminal() {
            conn.execute(
                "UPDATE sync_outbound SET state = ?1, next_retry_at = NULL WHERE id = ?2",
                params![s.as_str(), id],
            )
        } else {
            conn.execute(
                "UPDATE sync_outbound SET state = ?1 WHERE id = ?2",
                params![s.as_str(), id],
            )
        }
        .with_context(|| format!("set state {} for outbound {id}", s.as_str()))?;
        Ok(())
    }

    fn set_last_error(&self, id: i64, err: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        conn.execute(
            "UPDATE sync_outbound SET last_error = ?1 WHERE id = ?2",
            params![err, id],
        )
        .with_context(|| format!("set last_error for outbound {id}"))?;
        Ok(())
    }

    fn set_next_retry_at(&self, id: i64, at: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        conn.execute(
            "UPDATE sync_outbound SET next_retry_at = ?1 WHERE id = ?2",
            params![at, id],
        )
        .with_context(|| format!("set next_retry_at for outbound {id}"))?;
        Ok(())
    }

    fn set_wire_package_id(&self, id: i64, wire_id: &str) -> Result<()> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        conn.execute(
            "UPDATE sync_outbound SET wire_package_id = ?1 WHERE id = ?2",
            params![wire_id, id],
        )
        .with_context(|| format!("set wire_package_id for outbound {id}"))?;
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
                 WHERE state NOT IN ('confirmed', 'failed', 'cancelled')
                 ORDER BY id ASC"
            ))
            .context("prepare non_terminal")?;
        let raws = stmt
            .query_map([], outbound_raw_from_row)
            .context("query non_terminal")?
            .collect::<rusqlite::Result<Vec<OutboundRaw>>>()
            .context("collect non_terminal")?;
        raws.into_iter().map(to_outbound).collect()
    }

    fn get_outbound(&self, id: i64) -> Result<Option<OutboundRow>> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        outbound_row_by_id(&conn, id)
    }

    fn confirmed(&self) -> Result<Vec<OutboundRow>> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        confirmed_outbound_rows(&conn)
    }

    fn confirm(&self, id: i64, receipts: &[FrameReceipt]) -> Result<()> {
        // Task 4.1 (candidate (b)): time the confirm write — the sender's heaviest
        // store write, where SQLite contention with the receiver's separate
        // connection would surface. Instrumentation only — no behavior change.
        let write_started = std::time::Instant::now();
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        // Idempotent: only a non-confirmed row transitions (guards a duplicate
        // ack that slipped past the engine's in-flight map).
        let changed = conn
            .execute(
                "UPDATE sync_outbound SET state = ?1, confirmed_at = ?2, last_error = NULL, next_retry_at = NULL
                 WHERE id = ?3 AND state <> ?1",
                params![OutboundState::Confirmed.as_str(), now_iso(), id],
            )
            .with_context(|| format!("confirm outbound {id}"))?;
        let write_ms = write_started.elapsed().as_millis();
        if write_ms > crate::sync::SLOW_STORE_WRITE_MS {
            tracing::warn!(
                op = "confirm",
                duration_ms = write_ms as u64,
                "sync store write slow"
            );
        }
        tracing::debug!(
            package_id = id,
            receipts = receipts.len(),
            changed,
            "sync store confirm"
        );
        Ok(())
    }

    fn reset_outbound_for_resend(&self, id: i64, new_wire_id: &str) -> Result<()> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        reset_outbound_for_resend(&conn, id, new_wire_id)
    }

    fn set_outbound_project_id(&self, id: i64, project_id: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().expect("sync store mutex poisoned");
        set_outbound_project_id(&conn, id, project_id)
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
        // Transfers Batch Model reset (spec §Migration): MUST run before any DDL
        // below — see the standalone `open` for the detector-placement rationale
        // (checking table/column presence after `CREATE TABLE IF NOT EXISTS` has
        // run can no longer tell "fresh" from "legacy"). Here all eight transfer
        // tables may already exist on a legacy catalog, so the wipe reaches
        // receipts/sources too. (`init_db` normally runs this same guard first on
        // the pooled connection; this is defense-in-depth for this store's own
        // second connection.)
        wipe_transfer_bookkeeping_on_upgrade(&conn)?;
        conn.execute(DDL_OUTBOUND, [])
            .context("create sync_outbound")?;
        ensure_outbound_columns(&conn)?;
        conn.execute(DDL_HISTORY, [])
            .context("create sync_history")?;
        ensure_history_columns(&conn)?;
        conn.execute(DDL_RECEIPTS, [])
            .context("create sync_receipts")?;
        conn.execute(DDL_SYNC_SOURCES, [])
            .context("create sync_sources")?;
        conn.execute(DDL_INBOUND, [])
            .context("create sync_inbound")?;
        ensure_inbound_columns(&conn)?;
        conn.execute(DDL_OUTBOUND_FILES, [])
            .context("create sync_outbound_files")?;
        conn.execute(DDL_INBOUND_FILES, [])
            .context("create sync_inbound_files")?;
        conn.execute(DDL_SYNC_EVENTS, [])
            .context("create sync_events")?;
        for idx in DDL_INDEXES {
            conn.execute(idx, []).context("create sync index")?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Borrow the underlying connection for a synchronous unit of work (the
    /// receiver's per-frame ingest transaction). The guard must never be held
    /// across an `.await` — ingest is fully synchronous, so it isn't.
    ///
    /// It must also never be held across a whole *package*: ingest takes it
    /// **per frame** via [`IngestConn::Shared`](crate::sync::ingest::IngestConn)
    /// (W2 T2.1), so every other user of this connection — a concurrent
    /// transfer's fetch-sink state writes, its own ingest, the receiver's
    /// bookkeeping — waits at most one frame's hash + copy + transaction rather
    /// than the minutes a multi-GB package takes.
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

    /// Catalog files whose SAMPLING hash (`files.content_hash`) is one of
    /// `hashes` — the membership probe the P2P dedup responder
    /// ([`CatalogDedupResponder`](super::responder::CatalogDedupResponder))
    /// uses to split an incoming offer into definite-wants vs. sampling-hash
    /// candidates, each row carrying what the full-hash confirm then needs.
    /// Thin wrapper over
    /// [`find_files_by_content_hashes`](crate::db::find_files_by_content_hashes).
    pub fn files_by_sampling_hashes(
        &self,
        hashes: &[String],
    ) -> Result<Vec<crate::db::SamplingHashMatch>> {
        let conn = self.lock_conn();
        crate::db::find_files_by_content_hashes(&conn, hashes)
            .map_err(|e| anyhow::anyhow!("look up files by sampling hash: {e}"))
    }

    /// Bank a full-content hash the dedup confirm just read into
    /// `files.strong_hash`. The responder owns the staleness check; this is
    /// the locked write. Thin wrapper over
    /// [`bank_strong_hash`](crate::db::bank_strong_hash).
    pub fn bank_strong_hash(&self, file_id: i64, hash: &str) -> Result<()> {
        let conn = self.lock_conn();
        crate::db::bank_strong_hash(&conn, file_id, hash)
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("bank strong_hash for file {file_id}: {e}"))
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
    fn enqueue(
        &self,
        package_ref: &str,
        peer: NodeId,
        display_name: Option<&str>,
        files: &[AnnounceFileEntry],
        layout: PackageLayout,
    ) -> Result<i64> {
        let conn = self.lock_conn();
        let peer_hex = node_id_hex(&peer);
        insert_outbound_with_files(&conn, package_ref, &peer_hex, display_name, files, layout)
    }

    fn replace_outbound_files(&self, outbound_id: i64, files: &[OutboundFileRow]) -> Result<()> {
        let conn = self.lock_conn();
        replace_outbound_files(&conn, outbound_id, files)
    }

    fn list_outbound_files(&self, outbound_id: i64) -> Result<Vec<OutboundFileRow>> {
        let conn = self.lock_conn();
        list_outbound_files(&conn, outbound_id)
    }

    fn set_outbound_file_state(
        &self,
        outbound_id: i64,
        rel_path: &str,
        state: OutboundFileState,
        bytes_done: u64,
        outcome: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let conn = self.lock_conn();
        set_outbound_file_state(
            &conn,
            outbound_id,
            rel_path,
            state,
            bytes_done,
            outcome,
            error,
        )
    }

    fn append_sync_event(
        &self,
        direction: Direction,
        batch_key: &str,
        kind: &str,
        detail: Option<&str>,
    ) -> Result<()> {
        let conn = self.lock_conn();
        append_sync_event(&conn, direction, batch_key, kind, detail)
    }

    fn set_state(&self, id: i64, s: OutboundState) -> Result<()> {
        let conn = self.lock_conn();
        // Defense-in-depth (Task 2 M1): a terminal state has no pending retry,
        // so clear any persisted countdown in the same write — a future
        // non-engine caller can't leave a stale `next_retry_at` behind. The
        // engine also clears it explicitly; that duplicate is harmless.
        if s.is_terminal() {
            conn.execute(
                "UPDATE sync_outbound SET state = ?1, next_retry_at = NULL WHERE id = ?2",
                params![s.as_str(), id],
            )
        } else {
            conn.execute(
                "UPDATE sync_outbound SET state = ?1 WHERE id = ?2",
                params![s.as_str(), id],
            )
        }
        .with_context(|| format!("set state {} for outbound {id}", s.as_str()))?;
        Ok(())
    }

    fn set_last_error(&self, id: i64, err: Option<&str>) -> Result<()> {
        let conn = self.lock_conn();
        conn.execute(
            "UPDATE sync_outbound SET last_error = ?1 WHERE id = ?2",
            params![err, id],
        )
        .with_context(|| format!("set last_error for outbound {id}"))?;
        Ok(())
    }

    fn set_next_retry_at(&self, id: i64, at: Option<&str>) -> Result<()> {
        let conn = self.lock_conn();
        conn.execute(
            "UPDATE sync_outbound SET next_retry_at = ?1 WHERE id = ?2",
            params![at, id],
        )
        .with_context(|| format!("set next_retry_at for outbound {id}"))?;
        Ok(())
    }

    fn set_wire_package_id(&self, id: i64, wire_id: &str) -> Result<()> {
        let conn = self.lock_conn();
        conn.execute(
            "UPDATE sync_outbound SET wire_package_id = ?1 WHERE id = ?2",
            params![wire_id, id],
        )
        .with_context(|| format!("set wire_package_id for outbound {id}"))?;
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
                 WHERE state NOT IN ('confirmed', 'failed', 'cancelled')
                 ORDER BY id ASC"
            ))
            .context("prepare non_terminal")?;
        let raws = stmt
            .query_map([], outbound_raw_from_row)
            .context("query non_terminal")?
            .collect::<rusqlite::Result<Vec<OutboundRaw>>>()
            .context("collect non_terminal")?;
        raws.into_iter().map(to_outbound).collect()
    }

    fn get_outbound(&self, id: i64) -> Result<Option<OutboundRow>> {
        let conn = self.lock_conn();
        outbound_row_by_id(&conn, id)
    }

    fn confirmed(&self) -> Result<Vec<OutboundRow>> {
        let conn = self.lock_conn();
        confirmed_outbound_rows(&conn)
    }

    fn confirm(&self, id: i64, receipts: &[FrameReceipt]) -> Result<()> {
        // Task 4.1 (candidate (b)): time the confirm write — see the standalone
        // store's `confirm` for the contention rationale. No behavior change.
        let write_started = std::time::Instant::now();
        let conn = self.lock_conn();
        let changed = conn
            .execute(
                "UPDATE sync_outbound SET state = ?1, confirmed_at = ?2, last_error = NULL, next_retry_at = NULL
                 WHERE id = ?3 AND state <> ?1",
                params![OutboundState::Confirmed.as_str(), now_iso(), id],
            )
            .with_context(|| format!("confirm outbound {id}"))?;
        let write_ms = write_started.elapsed().as_millis();
        if write_ms > crate::sync::SLOW_STORE_WRITE_MS {
            tracing::warn!(
                op = "confirm",
                duration_ms = write_ms as u64,
                "sync store write slow"
            );
        }
        tracing::debug!(
            package_id = id,
            receipts = receipts.len(),
            changed,
            "catalog sync store confirm"
        );
        Ok(())
    }

    fn reset_outbound_for_resend(&self, id: i64, new_wire_id: &str) -> Result<()> {
        let conn = self.lock_conn();
        reset_outbound_for_resend(&conn, id, new_wire_id)
    }

    fn set_outbound_project_id(&self, id: i64, project_id: Option<&str>) -> Result<()> {
        let conn = self.lock_conn();
        set_outbound_project_id(&conn, id, project_id)
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
        let queued = store
            .enqueue("pkg-queued", PEER, None, &[], PackageLayout::Batch)
            .unwrap();
        let transferring = store
            .enqueue("pkg-transferring", PEER, None, &[], PackageLayout::Batch)
            .unwrap();
        let confirmed = store
            .enqueue("pkg-confirmed", PEER, None, &[], PackageLayout::Batch)
            .unwrap();
        store
            .set_state(transferring, OutboundState::Transferring)
            .unwrap();
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

    /// Task 4: a `Cancelled` receipt round-trips through the DB text encoding
    /// (`insert_receipt` → `load_receipts`) unchanged, AND is counted as
    /// *satisfied* by [`count_satisfied_receipts`]. A deliberate receiver "no" is
    /// a settled verdict (like ingested/duplicate, unlike a pending `Rejected`),
    /// so the receiver's ack-replay guard fires on a re-announce instead of
    /// re-fetching a frame the receiver already declined.
    #[test]
    fn receipt_outcome_cancelled_roundtrips_and_counts_satisfied() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(DDL_RECEIPTS, []).unwrap();

        let receipt = FrameReceipt {
            frame_uuid: "uuid-cancel".to_string(),
            xxh3: "hhhh".to_string(),
            outcome: ReceiptOutcome::Cancelled,
        };
        insert_receipt(&conn, "pkg-cancel", &receipt, "2026-07-15T00:00:00Z").unwrap();

        // Round-trips through the `cancelled` text encoding unchanged (a plain
        // `from_db` arm, NOT the `Rejected` catch-all).
        let loaded = load_receipts(&conn, "pkg-cancel").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].frame_uuid, "uuid-cancel");
        assert_eq!(loaded[0].outcome, ReceiptOutcome::Cancelled);

        // Counted as satisfied (the `outcome NOT LIKE 'rejected:%'` guard) so the
        // §4 ack-replay fires on re-announce.
        assert_eq!(count_satisfied_receipts(&conn, "pkg-cancel").unwrap(), 1);
    }

    /// Task 11: one inbound package walks the full lifecycle through the
    /// `sync_inbound` free fns — upsert lands `Announced`; `Fetching` + a
    /// `bytes_done` write persist; a terminal `Done` stamps `finished_at` and
    /// drops out of `inbound_active`; and a re-announce of a `Cancelled` row is a
    /// no-op (Cancelled is final).
    #[test]
    fn inbound_row_lifecycle() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(DDL_INBOUND, []).unwrap();
        let peer = "1122334455667788112233445566778811223344556677881122334455667788";
        let pkg = "pkg-inbound-1";

        // upsert → Announced, counts recorded, no bytes yet, active.
        let id = upsert_inbound_announced(&conn, peer, pkg, 5, 1000).unwrap();
        let row = get_inbound(&conn, pkg).unwrap().unwrap();
        assert_eq!(row.id, id);
        assert_eq!(row.peer, peer);
        assert_eq!(row.state, InboundState::Announced);
        assert_eq!(row.frame_count, 5);
        assert_eq!(row.byte_size, 1000);
        assert_eq!(row.bytes_done, 0);
        assert!(row.finished_at.is_none());
        assert_eq!(
            inbound_active(&conn).unwrap().len(),
            1,
            "announced is active"
        );

        // Fetching + a live byte update.
        set_inbound_state(&conn, pkg, InboundState::Fetching, None).unwrap();
        set_inbound_bytes_done(&conn, pkg, 100).unwrap();
        let row = get_inbound(&conn, pkg).unwrap().unwrap();
        assert_eq!(row.state, InboundState::Fetching);
        assert_eq!(row.bytes_done, 100);
        assert!(row.finished_at.is_none());
        assert_eq!(
            inbound_active(&conn).unwrap().len(),
            1,
            "fetching is still active"
        );

        // Done → finished_at stamped, gone from the active set.
        set_inbound_state(&conn, pkg, InboundState::Done, None).unwrap();
        let row = get_inbound(&conn, pkg).unwrap().unwrap();
        assert_eq!(row.state, InboundState::Done);
        assert!(
            row.finished_at.is_some(),
            "a terminal state stamps finished_at"
        );
        assert!(
            inbound_active(&conn).unwrap().is_empty(),
            "done is not active"
        );

        // Cancel, then re-announce: the cancelled row is untouched (final).
        set_inbound_state(&conn, pkg, InboundState::Cancelled, Some("declined")).unwrap();
        let id2 = upsert_inbound_announced(&conn, peer, pkg, 9, 2000).unwrap();
        assert_eq!(id2, id, "re-announce hits the same row");
        let row = get_inbound(&conn, pkg).unwrap().unwrap();
        assert_eq!(
            row.state,
            InboundState::Cancelled,
            "a cancelled row never resets to announced"
        );
        assert_eq!(
            row.frame_count, 5,
            "a cancelled row is a full no-op — counts not refreshed"
        );
        assert_eq!(row.byte_size, 1000);
        assert_eq!(
            row.last_error.as_deref(),
            Some("declined"),
            "cancel reason preserved"
        );
    }

    /// Perseus UI v2 (Task 9): a fresh inbound row has a NULL `peer_capability`;
    /// [`set_inbound_peer_capability`] persists the announcing peer's capability
    /// and it round-trips through the `INBOUND_COLS` read path.
    #[test]
    fn peer_capability_roundtrips_and_defaults_null() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(DDL_INBOUND, []).unwrap();
        let peer = "aa".repeat(32);
        let pkg = "pkg-cap-1";

        let id = upsert_inbound_announced(&conn, &peer, pkg, 2, 200).unwrap();
        let row = get_inbound(&conn, pkg).unwrap().unwrap();
        assert_eq!(
            row.peer_capability, None,
            "a fresh row has no capability stamp"
        );

        set_inbound_peer_capability(&conn, id, "perseus").unwrap();
        let row = get_inbound(&conn, pkg).unwrap().unwrap();
        assert_eq!(
            row.peer_capability.as_deref(),
            Some("perseus"),
            "capability persisted"
        );

        // Re-stamp overwrites in place (an attempt from a since-reclassified device).
        set_inbound_peer_capability(&conn, id, "athenaeum").unwrap();
        assert_eq!(
            get_inbound(&conn, pkg)
                .unwrap()
                .unwrap()
                .peer_capability
                .as_deref(),
            Some("athenaeum"),
        );
    }

    /// `next_retry_at` round-trips through a write + read and clears with `None`
    /// (Task 2): the engine persists the wall-clock retry deadline so the UI can
    /// render a countdown, and clears it on announce success / terminal — proving
    /// both the new column's SQL and the `Option` clear path.
    #[test]
    fn next_retry_at_roundtrips_and_clears() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StandaloneSyncStore::open(tmp.path().join("s.db")).unwrap();
        let id = store
            .enqueue("/tmp/pkg", [7u8; 32], None, &[], PackageLayout::Batch)
            .unwrap();
        store
            .set_next_retry_at(id, Some("2026-07-15T12:00:00Z"))
            .unwrap();
        let row = store.non_terminal().unwrap().pop().unwrap();
        assert_eq!(row.next_retry_at.as_deref(), Some("2026-07-15T12:00:00Z"));
        store.set_next_retry_at(id, None).unwrap();
        assert!(store
            .non_terminal()
            .unwrap()
            .pop()
            .unwrap()
            .next_retry_at
            .is_none());
    }

    /// `last_error` round-trips (Task 9): a fresh row carries `None`; a failed
    /// attempt writes it; an explicit clear or a `confirm` wipes it — proving both
    /// the new column's insert/read SQL and the confirm-clears invariant.
    #[test]
    fn last_error_writes_on_fail_and_clears_on_success() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap();

        let id = store
            .enqueue("pkg", PEER, None, &[], PackageLayout::Batch)
            .unwrap();
        assert_eq!(
            store.get_outbound(id).unwrap().unwrap().last_error,
            None,
            "a fresh row has no last_error"
        );

        // Written on a failed attempt.
        store
            .set_last_error(id, Some("serve package: peer not started"))
            .unwrap();
        assert_eq!(
            store
                .get_outbound(id)
                .unwrap()
                .unwrap()
                .last_error
                .as_deref(),
            Some("serve package: peer not started"),
        );

        // Explicit clear on a successful re-announce.
        store.set_last_error(id, None).unwrap();
        assert_eq!(store.get_outbound(id).unwrap().unwrap().last_error, None);

        // Re-write, then confirm clears it in the same UPDATE — a confirmed row
        // never carries a stale error.
        store
            .set_last_error(id, Some("no ack from peer within timeout"))
            .unwrap();
        assert_eq!(
            store
                .get_outbound(id)
                .unwrap()
                .unwrap()
                .last_error
                .as_deref(),
            Some("no ack from peer within timeout"),
        );
        store.confirm(id, &[]).unwrap();
        let row = store.get_outbound(id).unwrap().unwrap();
        assert_eq!(row.state, OutboundState::Confirmed);
        assert_eq!(row.last_error, None, "confirm must clear last_error");
    }

    /// Migration (Task 9): opening a store over a PRE-EXISTING `sync_outbound` table
    /// with no `last_error` column adds it in place, and a subsequent write/read of
    /// the column round-trips — proving both the guarded `ALTER TABLE` and the new
    /// column SQL on a legacy DB.
    #[test]
    fn opening_over_legacy_outbound_migrates_last_error() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("legacy.db");

        // Materialise the OLD (last_error-less) sync_outbound shape by hand.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "CREATE TABLE sync_outbound (
                    id INTEGER PRIMARY KEY,
                    package_ref TEXT NOT NULL,
                    peer TEXT NOT NULL,
                    state TEXT NOT NULL,
                    attempts INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL,
                    confirmed_at TEXT
                )",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sync_outbound (package_ref, peer, state, attempts, created_at)
                 VALUES ('pkg-old', ?1, 'queued', 0, 't0')",
                params![node_id_hex(&PEER)],
            )
            .unwrap();
            // Stand in for "this DB is already on the batch model" (`sync_inbound`
            // present with `batch_uuid`) so opening below does NOT trigger the
            // Transfers Batch Model upgrade wipe — this fixture pins the Task-9
            // `last_error` column migration in isolation, not the wipe.
            conn.execute(
                "CREATE TABLE sync_inbound (id INTEGER PRIMARY KEY, peer TEXT NOT NULL, \
                 package_id TEXT NOT NULL, state TEXT NOT NULL, batch_uuid TEXT, \
                 last_error TEXT, created_at TEXT NOT NULL DEFAULT '', finished_at TEXT, \
                 UNIQUE(peer, package_id))",
                [],
            )
            .unwrap();
        }

        // Opening the store migrates the table in place (no error).
        let store = StandaloneSyncStore::open(&db_path).unwrap();
        let rows = store.all_outbound(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].last_error, None,
            "legacy row's last_error migrated to NULL"
        );

        // The new column accepts a write + reads it back.
        store.set_last_error(rows[0].id, Some("boom")).unwrap();
        assert_eq!(
            store
                .get_outbound(rows[0].id)
                .unwrap()
                .unwrap()
                .last_error
                .as_deref(),
            Some("boom"),
        );
    }

    /// Migration (AUDIT B3): opening a store over a PRE-EXISTING project-less
    /// `sync_history` table adds the `project` column in place, and an insert
    /// carrying a project value round-trips through search — proving both the
    /// `ALTER TABLE` migration and the new column's insert/search SQL.
    #[test]
    fn opening_over_projectless_history_migrates_and_inserts() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("legacy.db");

        // Materialise the OLD (project-less) sync_history shape by hand — exactly
        // what a store created before Stage II left on disk.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "CREATE TABLE sync_history (
                    id INTEGER PRIMARY KEY,
                    frame_uuid TEXT, filename TEXT, object TEXT, peer_device TEXT,
                    direction TEXT, bytes INTEGER, started_at TEXT, finished_at TEXT, outcome TEXT
                )",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sync_history
                    (frame_uuid, filename, object, peer_device, direction, bytes, started_at, finished_at, outcome)
                 VALUES ('u-old', 'old.fits', 'M42', 'peerA', 'sent', 10, 't0', NULL, 'sent')",
                [],
            )
            .unwrap();
            // Stand in for "this DB is already on the batch model" so opening below
            // does NOT trigger the Transfers Batch Model upgrade wipe — this fixture
            // pins the AUDIT-B3 `project` column migration in isolation.
            conn.execute(
                "CREATE TABLE sync_inbound (id INTEGER PRIMARY KEY, peer TEXT NOT NULL, \
                 package_id TEXT NOT NULL, state TEXT NOT NULL, batch_uuid TEXT, \
                 last_error TEXT, created_at TEXT NOT NULL DEFAULT '', finished_at TEXT, \
                 UNIQUE(peer, package_id))",
                [],
            )
            .unwrap();
        }

        // Opening the store migrates the table in place (no error) …
        let store = StandaloneSyncStore::open(&db_path).unwrap();

        // … a legacy row now reads back with project = NULL …
        let legacy = store
            .search_history(HistoryQuery {
                filename: Some("old.fits".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(legacy.len(), 1);
        assert_eq!(
            legacy[0].project, None,
            "legacy row's project migrated to NULL"
        );

        // … and a fresh insert carrying a project value round-trips + filters.
        store
            .append_history(HistoryRow {
                frame_uuid: "u-new".into(),
                filename: "new.fits".into(),
                object: Some("M31".into()),
                peer_device: "peerB".into(),
                direction: Direction::Received,
                bytes: 20,
                started_at: "t1".into(),
                finished_at: Some("t2".into()),
                outcome: "ingested".into(),
                project: Some("proj-42".into()),
                package_id: None,
                batch_name: None,
            })
            .unwrap();

        let by_project = store
            .search_history(HistoryQuery {
                project: Some("proj-42".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            by_project.len(),
            1,
            "project filter matches exactly the collab row"
        );
        assert_eq!(by_project[0].frame_uuid, "u-new");
        assert_eq!(by_project[0].project.as_deref(), Some("proj-42"));

        // A project filter that matches nothing returns empty (never the legacy row).
        assert!(store
            .search_history(HistoryQuery {
                project: Some("proj-none".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap()
            .is_empty());
    }

    /// Task 14: opening a store over a PRE-EXISTING `sync_history` table with no
    /// `package_id` column adds it in place; a legacy row still loads (with
    /// `package_id = None`); and a fresh insert carrying a `package_id` round-trips
    /// through both a plain search AND the new `package_id` batch-key filter —
    /// proving the guarded `ALTER TABLE`, the new column's insert/read SQL, and the
    /// per-batch detail filter [`list_transfer_files`](crate::api::sync::list_transfer_files)
    /// relies on.
    #[test]
    fn history_package_id_migrates_roundtrips_and_filters() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("legacy.db");

        // Materialise a pre-Task-14 `sync_history` shape (project column present,
        // package_id absent) and a legacy row that predates package_id.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute(
                "CREATE TABLE sync_history (
                    id INTEGER PRIMARY KEY,
                    frame_uuid TEXT, filename TEXT, object TEXT, peer_device TEXT,
                    direction TEXT, bytes INTEGER, started_at TEXT, finished_at TEXT,
                    outcome TEXT, project TEXT
                )",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sync_history
                    (frame_uuid, filename, object, peer_device, direction, bytes, started_at, finished_at, outcome, project)
                 VALUES ('u-old', 'old.fits', 'M42', 'peerA', 'sent', 10, 't0', NULL, 'sent', NULL)",
                [],
            )
            .unwrap();
            // Stand in for "this DB is already on the batch model" so opening below
            // does NOT trigger the Transfers Batch Model upgrade wipe — this fixture
            // pins the Task-14 `package_id` column migration in isolation.
            conn.execute(
                "CREATE TABLE sync_inbound (id INTEGER PRIMARY KEY, peer TEXT NOT NULL, \
                 package_id TEXT NOT NULL, state TEXT NOT NULL, batch_uuid TEXT, \
                 last_error TEXT, created_at TEXT NOT NULL DEFAULT '', finished_at TEXT, \
                 UNIQUE(peer, package_id))",
                [],
            )
            .unwrap();
        }

        // Opening migrates the table in place (adds package_id); the legacy row
        // loads with package_id = None.
        let store = StandaloneSyncStore::open(&db_path).unwrap();
        let legacy = store
            .search_history(HistoryQuery {
                filename: Some("old.fits".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(legacy.len(), 1);
        assert_eq!(
            legacy[0].package_id, None,
            "legacy row's package_id migrated to NULL"
        );

        // A fresh insert carrying a package_id round-trips …
        store
            .append_history(HistoryRow {
                frame_uuid: "u-new".into(),
                filename: "new.fits".into(),
                object: Some("M31".into()),
                peer_device: "peerB".into(),
                direction: Direction::Sent,
                bytes: 20,
                started_at: "t1".into(),
                finished_at: Some("t2".into()),
                outcome: "ingested".into(),
                project: None,
                package_id: Some("pkg-abc".into()),
                batch_name: None,
            })
            .unwrap();

        // … and the new package_id filter matches exactly that batch, never the
        // legacy NULL row.
        let by_pkg = store
            .search_history(HistoryQuery {
                package_id: Some("pkg-abc".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            by_pkg.len(),
            1,
            "package_id filter matches exactly the tagged row"
        );
        assert_eq!(by_pkg[0].frame_uuid, "u-new");
        assert_eq!(by_pkg[0].package_id.as_deref(), Some("pkg-abc"));

        assert!(store
            .search_history(HistoryQuery {
                package_id: Some("pkg-none".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap()
            .is_empty());
    }

    // ── Transfers Status Model v2 (tv2 Task 2) ──────────────────────────────

    /// Sorted column names of `table` — a local PRAGMA helper for the migration
    /// assertions below.
    fn cols(conn: &Connection, table: &str) -> Vec<String> {
        let mut v: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        v.sort();
        v
    }

    fn table_exists(conn: &Connection, table: &str) -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |_| Ok(()),
        )
        .optional()
        .unwrap()
        .is_some()
    }

    /// tv2 §D1/§D4/§D7: a fresh `open` materialises every new column and table,
    /// and a second `open` over the same file is a clean no-op.
    #[test]
    fn fresh_open_creates_v2_tables_and_columns_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sync.db");
        StandaloneSyncStore::open(&path).unwrap();
        // Re-open the same file: idempotent (every DDL is `IF NOT EXISTS`, every
        // guarded ALTER a no-op on an already-migrated table).
        StandaloneSyncStore::open(&path).unwrap();

        let conn = Connection::open(&path).unwrap();
        for t in ["sync_outbound_files", "sync_inbound_files", "sync_events"] {
            assert!(
                table_exists(&conn, t),
                "{t} must be created on a fresh open"
            );
        }
        assert!(cols(&conn, "sync_outbound").contains(&"display_name".to_string()));
        assert!(cols(&conn, "sync_inbound").contains(&"display_name".to_string()));
        assert!(cols(&conn, "sync_history").contains(&"batch_name".to_string()));
    }

    /// tv2 §D1: a simulated 0.4.x DB (pre-change `sync_outbound` / `sync_inbound`
    /// / `sync_history` shapes) gains `display_name` / `batch_name` when the
    /// guarded-ALTER migration fns run — and running them again is a no-op.
    #[test]
    fn legacy_db_migrates_v2_columns_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        // PRE-change DDL, copied verbatim from the CREATE statements before this
        // cycle (sync_inbound shipped in beta.3 WITHOUT display_name).
        conn.execute(
            "CREATE TABLE sync_outbound (
                id INTEGER PRIMARY KEY, package_ref TEXT NOT NULL, peer TEXT NOT NULL,
                state TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL,
                confirmed_at TEXT, last_error TEXT, next_retry_at TEXT, wire_package_id TEXT
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE sync_inbound (
                id INTEGER PRIMARY KEY, peer TEXT NOT NULL, package_id TEXT NOT NULL,
                state TEXT NOT NULL, frame_count INTEGER NOT NULL DEFAULT 0,
                byte_size INTEGER NOT NULL DEFAULT 0, bytes_done INTEGER NOT NULL DEFAULT 0,
                last_error TEXT, created_at TEXT NOT NULL, finished_at TEXT,
                UNIQUE(peer, package_id)
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE sync_history (
                id INTEGER PRIMARY KEY, frame_uuid TEXT, filename TEXT, object TEXT,
                peer_device TEXT, direction TEXT, bytes INTEGER, started_at TEXT,
                finished_at TEXT, outcome TEXT, project TEXT, package_id TEXT
            )",
            [],
        )
        .unwrap();

        // Absent before migration.
        assert!(!cols(&conn, "sync_outbound").contains(&"display_name".to_string()));
        assert!(!cols(&conn, "sync_inbound").contains(&"display_name".to_string()));
        assert!(!cols(&conn, "sync_history").contains(&"batch_name".to_string()));

        // Migrate.
        ensure_outbound_columns(&conn).unwrap();
        ensure_inbound_columns(&conn).unwrap();
        ensure_history_columns(&conn).unwrap();

        assert!(cols(&conn, "sync_outbound").contains(&"display_name".to_string()));
        assert!(cols(&conn, "sync_inbound").contains(&"display_name".to_string()));
        assert!(cols(&conn, "sync_history").contains(&"batch_name".to_string()));

        // Idempotent: a second pass never errors (an already-present column is a
        // no-op) and leaves the columns in place.
        ensure_outbound_columns(&conn).unwrap();
        ensure_inbound_columns(&conn).unwrap();
        ensure_history_columns(&conn).unwrap();
        assert!(cols(&conn, "sync_inbound").contains(&"display_name".to_string()));
    }

    /// Mirror-hierarchy T1: a fresh DB carries `sync_outbound.layout`, and a row
    /// enqueued with [`PackageLayout::Batch`] reads back as `Batch` — the write
    /// path round-trips the batch stamp. (The DDL's own `'batch'` default is
    /// covered by the raw-INSERT/migration test
    /// [`legacy_outbound_row_back_fills_layout_batch`], which inserts without the
    /// column.) A row whose column holds `'mirror'` decodes through the same read
    /// path as [`PackageLayout::Mirror`], so the DB text repr is pinned end to end.
    #[test]
    fn outbound_layout_column_exists_and_defaults_to_batch() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sync.db");
        StandaloneSyncStore::open(&path).unwrap();
        let conn = Connection::open(&path).unwrap();
        assert!(cols(&conn, "sync_outbound").contains(&"layout".to_string()));

        // Enqueue with an explicit `Batch` — this pins the write path's batch
        // stamp, not the DDL default (that is the raw-INSERT/migration test).
        let id = insert_outbound_with_files(
            &conn,
            "/tmp/pkg-x",
            &node_id_hex(&PEER),
            None,
            &[],
            PackageLayout::Batch,
        )
        .unwrap();
        let row = outbound_row_by_id(&conn, id).unwrap().unwrap();
        assert_eq!(
            row.layout,
            PackageLayout::Batch,
            "an explicit Batch enqueue round-trips as batch"
        );

        conn.execute(
            "UPDATE sync_outbound SET layout = 'mirror' WHERE id = ?1",
            params![id],
        )
        .unwrap();
        let row = outbound_row_by_id(&conn, id).unwrap().unwrap();
        assert_eq!(
            row.layout,
            PackageLayout::Mirror,
            "'mirror' decodes on the read path"
        );
    }

    /// Mirror-hierarchy T1: a pre-mirror `sync_outbound` (no `layout` column) with
    /// a row already in it gains the column through the guarded ALTER, and that
    /// existing row back-fills to `batch` — an upgrade never re-labels a transfer
    /// the user already queued. Idempotent, like every other guarded column.
    #[test]
    fn legacy_outbound_row_back_fills_layout_batch() {
        let conn = Connection::open_in_memory().unwrap();
        // PRE-mirror-hierarchy DDL, copied from the CREATE statement before this
        // cycle (through `generation`, without `layout`).
        conn.execute(
            "CREATE TABLE sync_outbound (
                id INTEGER PRIMARY KEY, package_ref TEXT NOT NULL, peer TEXT NOT NULL,
                state TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL,
                confirmed_at TEXT, last_error TEXT, next_retry_at TEXT, wire_package_id TEXT,
                display_name TEXT, project_id TEXT, generation INTEGER NOT NULL DEFAULT 1
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sync_outbound (package_ref, peer, state, attempts, created_at)
             VALUES ('p', ?1, 'queued', 0, 't0')",
            params![node_id_hex(&PEER)],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        assert!(!cols(&conn, "sync_outbound").contains(&"layout".to_string()));

        ensure_outbound_columns(&conn).unwrap();
        assert!(cols(&conn, "sync_outbound").contains(&"layout".to_string()));
        assert_eq!(
            outbound_row_by_id(&conn, id).unwrap().unwrap().layout,
            PackageLayout::Batch,
            "a pre-existing row back-fills to batch"
        );

        // Idempotent: a second pass is a no-op, not an error.
        ensure_outbound_columns(&conn).unwrap();
        assert!(cols(&conn, "sync_outbound").contains(&"layout".to_string()));
    }

    /// Mirror-hierarchy T2: the layout the caller chose is stamped onto the row at
    /// INSERT — inside the enqueue transaction, not by a later UPDATE — so the row
    /// is never visible to the announce path without its landing shape. A resend
    /// reset (a targeted UPDATE of the state/attempt columns) must leave that stamp
    /// alone: an attempt re-sends the SAME transfer, and re-labelling its shape
    /// mid-flight would move files the receiver already landed.
    #[test]
    fn enqueue_stamps_layout_on_the_row() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sync.db");
        StandaloneSyncStore::open(&path).unwrap();
        let conn = Connection::open(&path).unwrap();

        let id = insert_outbound_with_files(
            &conn,
            "/tmp/pkg-m",
            &node_id_hex(&PEER),
            Some("Night"),
            &[],
            PackageLayout::Mirror,
        )
        .unwrap();
        assert_eq!(
            outbound_row_by_id(&conn, id).unwrap().unwrap().layout,
            PackageLayout::Mirror,
            "the stamp is written at insert"
        );

        // A resend reset (targeted UPDATE) must preserve the stamp.
        reset_outbound_for_resend(&conn, id, "new-wire-id").unwrap();
        assert_eq!(
            outbound_row_by_id(&conn, id).unwrap().unwrap().layout,
            PackageLayout::Mirror,
            "resend reset preserves layout"
        );

        // The other arm of the same path: a `Batch` caller stamps `batch`.
        let bid = insert_outbound_with_files(
            &conn,
            "/tmp/pkg-b",
            &node_id_hex(&PEER),
            None,
            &[],
            PackageLayout::Batch,
        )
        .unwrap();
        assert_eq!(
            outbound_row_by_id(&conn, bid).unwrap().unwrap().layout,
            PackageLayout::Batch,
        );
    }

    /// Mirror-hierarchy T2: the stamp travels through the trait too — the engine
    /// worker only ever reaches the column via [`SyncStore::enqueue`], so the
    /// delegation is pinned here and not only at the free-function seam.
    #[test]
    fn store_enqueue_stamps_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sync.db");
        let store = StandaloneSyncStore::open(&path).unwrap();

        let mirror = store
            .enqueue("pkg-mirror", PEER, None, &[], PackageLayout::Mirror)
            .unwrap();
        let batch = store
            .enqueue("pkg-batch", PEER, None, &[], PackageLayout::Batch)
            .unwrap();

        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            outbound_row_by_id(&conn, mirror).unwrap().unwrap().layout,
            PackageLayout::Mirror
        );
        assert_eq!(
            outbound_row_by_id(&conn, batch).unwrap().unwrap().layout,
            PackageLayout::Batch
        );
    }

    /// tv2 §D1: `display_name` write paths round-trip on both directions and clear
    /// with `None`; a fresh row carries `None`.
    #[test]
    fn display_name_setters_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(DDL_OUTBOUND, []).unwrap();
        conn.execute(DDL_INBOUND, []).unwrap();

        conn.execute(
            "INSERT INTO sync_outbound (package_ref, peer, state, attempts, created_at)
             VALUES ('p', ?1, 'queued', 0, 't0')",
            params![node_id_hex(&PEER)],
        )
        .unwrap();
        let oid = conn.last_insert_rowid();
        assert_eq!(
            outbound_row_by_id(&conn, oid)
                .unwrap()
                .unwrap()
                .display_name,
            None
        );
        set_outbound_display_name(&conn, oid, Some("M42 — 12 lights")).unwrap();
        assert_eq!(
            outbound_row_by_id(&conn, oid)
                .unwrap()
                .unwrap()
                .display_name
                .as_deref(),
            Some("M42 — 12 lights"),
        );
        set_outbound_display_name(&conn, oid, None).unwrap();
        assert_eq!(
            outbound_row_by_id(&conn, oid)
                .unwrap()
                .unwrap()
                .display_name,
            None
        );

        let iid = upsert_inbound_announced(&conn, &node_id_hex(&PEER), "pkg-1", 3, 300).unwrap();
        assert_eq!(
            get_inbound(&conn, "pkg-1").unwrap().unwrap().display_name,
            None
        );
        set_inbound_display_name(&conn, iid, Some("Downloads batch")).unwrap();
        assert_eq!(
            get_inbound(&conn, "pkg-1")
                .unwrap()
                .unwrap()
                .display_name
                .as_deref(),
            Some("Downloads batch"),
        );
    }

    /// tv2 §D4: outbound per-file CRUD — replace lands the set, per-file state
    /// upsert touches exactly one row, replace overwrites the whole set, and
    /// delete-with-parent removes one parent's rows only.
    #[test]
    fn outbound_file_rows_crud_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(DDL_OUTBOUND_FILES, []).unwrap();

        let mk = |rel: &str, size: u64| OutboundFileRow {
            outbound_id: 42,
            rel_path: rel.into(),
            byte_size: size,
            frame_uuid: format!("uuid-{rel}"),
            state: OutboundFileState::Pending,
            bytes_done: 0,
            outcome: None,
            error: None,
            updated_at: "2026-07-20T00:00:00.000Z".into(),
        };
        replace_outbound_files(&conn, 42, &[mk("a/y.fits", 200), mk("a/x.fits", 100)]).unwrap();
        // A sibling parent, to prove scoping.
        let sib = OutboundFileRow {
            outbound_id: 99,
            ..mk("z.fits", 5)
        };
        replace_outbound_files(&conn, 99, &[sib]).unwrap();

        let rows = list_outbound_files(&conn, 42).unwrap();
        assert_eq!(rows.len(), 2);
        // Ordered by rel_path.
        assert_eq!(rows[0].rel_path, "a/x.fits");
        assert_eq!(rows[0].byte_size, 100);
        assert_eq!(rows[0].frame_uuid, "uuid-a/x.fits");
        assert_eq!(rows[0].state, OutboundFileState::Pending);
        assert_eq!(rows[0].bytes_done, 0);

        // Per-file state upsert touches only a/x.fits.
        set_outbound_file_state(
            &conn,
            42,
            "a/x.fits",
            OutboundFileState::Uploaded,
            100,
            Some("ingested"),
            None,
        )
        .unwrap();
        let rows = list_outbound_files(&conn, 42).unwrap();
        let x = rows.iter().find(|r| r.rel_path == "a/x.fits").unwrap();
        assert_eq!(x.state, OutboundFileState::Uploaded);
        assert_eq!(x.bytes_done, 100);
        assert_eq!(x.outcome.as_deref(), Some("ingested"));
        let y = rows.iter().find(|r| r.rel_path == "a/y.fits").unwrap();
        assert_eq!(
            y.state,
            OutboundFileState::Pending,
            "sibling file untouched"
        );

        // Replace overwrites the whole set for parent 42.
        replace_outbound_files(&conn, 42, &[mk("only.fits", 7)]).unwrap();
        let rows = list_outbound_files(&conn, 42).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rel_path, "only.fits");

        // Delete-with-parent removes parent 42's rows; parent 99 untouched.
        delete_outbound_files(&conn, 42).unwrap();
        assert!(list_outbound_files(&conn, 42).unwrap().is_empty());
        assert_eq!(
            list_outbound_files(&conn, 99).unwrap().len(),
            1,
            "sibling parent unaffected"
        );
    }

    /// tv2 §D4: inbound per-file CRUD — the receive-side twin of the outbound
    /// roundtrip.
    #[test]
    fn inbound_file_rows_crud_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(DDL_INBOUND_FILES, []).unwrap();

        let mk = |rel: &str, size: u64| InboundFileRow {
            inbound_id: 7,
            rel_path: rel.into(),
            byte_size: size,
            frame_uuid: format!("uuid-{rel}"),
            state: InboundFileState::Announced,
            bytes_done: 0,
            outcome: None,
            error: None,
            updated_at: "2026-07-20T00:00:00.000Z".into(),
        };
        replace_inbound_files(&conn, 7, &[mk("f2.fits", 20), mk("f1.fits", 10)]).unwrap();
        replace_inbound_files(
            &conn,
            8,
            &[InboundFileRow {
                inbound_id: 8,
                ..mk("other.fits", 1)
            }],
        )
        .unwrap();

        let rows = list_inbound_files(&conn, 7).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].rel_path, "f1.fits");
        assert_eq!(rows[0].state, InboundFileState::Announced);

        set_inbound_file_state(
            &conn,
            7,
            "f1.fits",
            InboundFileState::Failed,
            3,
            None,
            Some("hash mismatch"),
        )
        .unwrap();
        let rows = list_inbound_files(&conn, 7).unwrap();
        let f1 = rows.iter().find(|r| r.rel_path == "f1.fits").unwrap();
        assert_eq!(f1.state, InboundFileState::Failed);
        assert_eq!(f1.bytes_done, 3);
        assert_eq!(f1.error.as_deref(), Some("hash mismatch"));

        delete_inbound_files(&conn, 7).unwrap();
        assert!(list_inbound_files(&conn, 7).unwrap().is_empty());
        assert_eq!(
            list_inbound_files(&conn, 8).unwrap().len(),
            1,
            "sibling parent unaffected"
        );
    }

    /// tv2 §D5: the grouped file-counts rollup — one query over a mixed table
    /// (pending / sending / uploaded / done / failed / rejected) yields correct,
    /// mutually-exclusive `done`/`failed` counts per parent, scopes to the
    /// requested id set, and omits an id with no per-file rows.
    #[test]
    fn grouped_file_counts_classifies_mixed_states() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(DDL_OUTBOUND_FILES, []).unwrap();

        // Parent 1: a realistic in-flight-then-terminal mix.
        let row = |parent: i64, rel: &str, state: OutboundFileState, outcome: Option<&str>| {
            OutboundFileRow {
                outbound_id: parent,
                rel_path: rel.into(),
                byte_size: 10,
                frame_uuid: format!("u-{parent}-{rel}"),
                state,
                bytes_done: 0,
                outcome: outcome.map(str::to_string),
                error: None,
                updated_at: "2026-07-20T00:00:00.000Z".into(),
            }
        };
        replace_outbound_files(
            &conn,
            1,
            &[
                row(1, "pending.fits", OutboundFileState::Pending, None),
                row(1, "sending.fits", OutboundFileState::Sending, None),
                // Uploaded with no outcome yet → counts as done (NULL outcome
                // must not poison the CASE).
                row(1, "uploaded.fits", OutboundFileState::Uploaded, None),
                row(
                    1,
                    "ingested.fits",
                    OutboundFileState::Done,
                    Some("ingested"),
                ),
                // Rejected receipt lands state=done + outcome=rejected:… → failed,
                // NOT done (mutual exclusivity).
                row(
                    1,
                    "rejected.fits",
                    OutboundFileState::Done,
                    Some("rejected:bad hash"),
                ),
                // Cancelled is terminal-but-not-error → done.
                row(
                    1,
                    "cancelled.fits",
                    OutboundFileState::Done,
                    Some("cancelled"),
                ),
                // §D4 negotiate-time exclusion: the peer already had it, settled
                // done/duplicate before a byte moved → done AND duplicate (a
                // subset of done, not a sibling).
                row(
                    1,
                    "duplicate.fits",
                    OutboundFileState::Done,
                    Some("duplicate"),
                ),
            ],
        )
        .unwrap();
        // Parent 2: a sibling, to prove IN-set scoping.
        replace_outbound_files(
            &conn,
            2,
            &[row(2, "only.fits", OutboundFileState::Pending, None)],
        )
        .unwrap();

        let counts = outbound_file_counts(&conn, &[1, 2, 999]).unwrap();
        let p1 = counts.get(&1).expect("parent 1 present");
        assert_eq!(p1.total, 7);
        // done = uploaded + ingested + cancelled + duplicate = 4.
        assert_eq!(p1.done, 4, "uploaded/ingested/cancelled/duplicate are done");
        // failed = rejected = 1.
        assert_eq!(p1.failed, 1, "rejected is failed, not done");
        // in-flight remainder = pending + sending = 7 - 4 - 1 = 2.
        assert_eq!(p1.total - p1.done - p1.failed, 2);
        // The split the sender's progress line subtracts: 1 file, its 10 bytes.
        assert_eq!(p1.duplicate, 1, "the duplicate row is counted inside done");
        assert_eq!(p1.duplicate_bytes, 10, "and its byte_size is summed");

        let p2 = counts.get(&2).unwrap();
        assert_eq!(p2.total, 1);
        assert_eq!((p2.duplicate, p2.duplicate_bytes), (0, 0));
        assert!(!counts.contains_key(&999), "an id with no rows is absent");

        // Empty id set → no query, empty map.
        assert!(outbound_file_counts(&conn, &[]).unwrap().is_empty());
    }

    /// D2 §3.4: the shared predicate must make the RECEIVE side climb too. The pin
    /// above is outbound-only, so a `fetched` term added without this test would
    /// pass while never being exercised inbound — which is exactly how the receiver
    /// ended up with a counter stuck at 0 of 38 while bytes visibly flowed.
    #[test]
    fn grouped_file_counts_classifies_mixed_inbound_states() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(DDL_INBOUND_FILES, []).unwrap();

        let ins = |rel: &str, state: &str, outcome: Option<&str>| {
            conn.execute(
                "INSERT INTO sync_inbound_files
                    (inbound_id, rel_path, byte_size, frame_uuid, state, bytes_done, outcome, updated_at)
                 VALUES (1, ?1, 10, ?2, ?3, 0, ?4, '2026-07-25T00:00:00.000Z')",
                params![rel, format!("u-{rel}"), state, outcome],
            )
            .unwrap();
        };
        ins("announced.fits", "announced", None);
        ins("fetching.fits", "fetching", None);
        // The new rung: bytes complete, verdict pending — the receive-side twin of
        // the sender's `uploaded`, counted through the same CASE.
        ins("fetched.fits", "fetched", None);
        ins("ingested.fits", "done", Some("ingested"));
        ins("failed.fits", "failed", None);
        ins("rejected.fits", "done", Some("rejected:bad hash"));
        // An ingest-time duplicate (the file travelled, the catalog already had
        // it): done, and reported in the shared `duplicate` columns.
        ins("duplicate.fits", "done", Some("duplicate"));

        let counts = inbound_file_counts(&conn, &[1]).unwrap();
        let c = counts.get(&1).expect("parent present");
        assert_eq!(c.total, 7);
        assert_eq!(
            c.done, 3,
            "fetched + ingested + duplicate — a rejected done is not done"
        );
        assert_eq!(c.failed, 2, "the failed row and the rejected-outcome row");
        assert_eq!(
            c.total - c.done - c.failed,
            2,
            "announced + fetching remain in flight"
        );
        assert_eq!((c.duplicate, c.duplicate_bytes), (1, 10));
    }

    /// D2 §3.4: `settle_unsettled_inbound_files` keys on `state <> 'done'`, so a
    /// `fetched` row settles at a terminal exactly like a `fetching` one — no
    /// change needed there. Pinned so a later edit cannot silently strand the new
    /// rung outside the batch-level closer.
    #[test]
    fn a_fetched_row_settles_at_terminal_like_a_fetching_one() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(DDL_INBOUND_FILES, []).unwrap();
        for (rel, st, outcome) in [
            ("a.fits", "fetching", None),
            ("b.fits", "fetched", None),
            ("c.fits", "done", Some("ingested")),
        ] {
            conn.execute(
                "INSERT INTO sync_inbound_files
                    (inbound_id, rel_path, byte_size, frame_uuid, state, bytes_done, outcome, updated_at)
                 VALUES (1, ?1, 10, ?2, ?3, 0, ?4, '2026-07-25T00:00:00.000Z')",
                params![rel, format!("u-{rel}"), st, outcome],
            )
            .unwrap();
        }

        settle_unsettled_inbound_files(&conn, 1, InboundFileState::Done, Some("cancelled"), None)
            .unwrap();

        let rows = list_inbound_files(&conn, 1).unwrap();
        assert!(
            rows.iter().all(|r| r.state == InboundFileState::Done),
            "fetching AND fetched both settle: {:?}",
            rows.iter()
                .map(|r| (r.rel_path.clone(), r.state))
                .collect::<Vec<_>>()
        );
        let already_done = rows.iter().find(|r| r.rel_path == "c.fits").unwrap();
        assert_eq!(
            already_done.outcome.as_deref(),
            Some("ingested"),
            "an already-done row keeps its own verdict"
        );
    }

    /// tv2 §D7: `append_sync_event` prunes each batch's journal to the newest
    /// [`SYNC_EVENTS_CAP`] rows — 205 inserts for one key leave exactly 200 (the
    /// newest), and other `(direction, batch_key)` journals are untouched.
    #[test]
    fn sync_events_append_prunes_to_cap() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(DDL_SYNC_EVENTS, []).unwrap();

        for i in 0..205 {
            append_sync_event(
                &conn,
                Direction::Sent,
                "7",
                "serve_start",
                Some(&format!("n{i}")),
            )
            .unwrap();
        }
        // Different journals (direction differs / batch_key differs) — unaffected
        // by the prune of (Sent, "7").
        append_sync_event(&conn, Direction::Received, "7", "fetch_start", None).unwrap();
        append_sync_event(&conn, Direction::Sent, "8", "serve_start", None).unwrap();

        let sent7 = list_sync_events(&conn, Direction::Sent, "7").unwrap();
        assert_eq!(
            sent7.len(),
            SYNC_EVENTS_CAP as usize,
            "cap keeps exactly the newest 200"
        );
        // Newest-first ordering; n0..n4 (the oldest 5) were pruned.
        assert_eq!(sent7[0].detail.as_deref(), Some("n204"), "newest first");
        assert_eq!(sent7[0].direction, Direction::Sent);
        assert_eq!(sent7[0].kind, "serve_start");
        assert_eq!(
            sent7.last().unwrap().detail.as_deref(),
            Some("n5"),
            "oldest survivor is n5"
        );

        assert_eq!(
            list_sync_events(&conn, Direction::Received, "7")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            list_sync_events(&conn, Direction::Sent, "8").unwrap().len(),
            1
        );

        // Delete-with-parent clears one journal only.
        delete_sync_events(&conn, Direction::Sent, "7").unwrap();
        assert!(list_sync_events(&conn, Direction::Sent, "7")
            .unwrap()
            .is_empty());
        assert_eq!(
            list_sync_events(&conn, Direction::Sent, "8").unwrap().len(),
            1
        );
    }

    // ── Transfers Batch Model (tvb B2) ──────────────────────────────────────

    /// Batch Model §Migration: a FRESH `open` is born with `batch_uuid` /
    /// `project_id` and the unique `(peer, batch_uuid)` index, so the upgrade wipe
    /// never fires; a second open is a clean no-op.
    #[test]
    fn fresh_open_has_batch_columns_index_and_never_wipes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sync.db");
        let store = StandaloneSyncStore::open(&path).unwrap();
        // A row present on the fresh DB must survive a re-open (no spurious wipe).
        let id = store
            .enqueue("pkg", PEER, None, &[], PackageLayout::Batch)
            .unwrap();
        drop(store);
        StandaloneSyncStore::open(&path).unwrap(); // second open — idempotent

        let conn = Connection::open(&path).unwrap();
        assert!(cols(&conn, "sync_inbound").contains(&"batch_uuid".to_string()));
        assert!(cols(&conn, "sync_inbound").contains(&"project_id".to_string()));
        assert!(cols(&conn, "sync_outbound").contains(&"project_id".to_string()));
        let idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_sync_inbound_batch'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            idx, 1,
            "unique (peer, batch_uuid) index present on a fresh open"
        );
        // The pre-existing outbound row was NOT wiped by the second open.
        assert!(
            outbound_row_by_id(&conn, id).unwrap().is_some(),
            "a fresh DB is born migrated — re-open must not wipe its rows",
        );
    }

    /// Batch Model §Migration: the upgrade wipe empties every present transfer
    /// table when `sync_inbound.batch_uuid` is absent, fires EXACTLY once (the
    /// column it back-fills is the flag), and skips a sender-only store's missing
    /// `sync_receipts` / `sync_sources` rather than erroring.
    #[test]
    fn upgrade_wipe_clears_present_tables_once_and_tolerates_missing() {
        let conn = Connection::open_in_memory().unwrap();
        // Legacy `sync_inbound` (no batch_uuid) + the sender-only table subset —
        // deliberately NO sync_receipts / sync_sources (Perseus shape).
        conn.execute(
            "CREATE TABLE sync_inbound (id INTEGER PRIMARY KEY, peer TEXT, package_id TEXT, state TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE sync_outbound (id INTEGER PRIMARY KEY, x TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE sync_outbound_files (id INTEGER PRIMARY KEY, x TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE sync_inbound_files (id INTEGER PRIMARY KEY, x TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE sync_events (id INTEGER PRIMARY KEY, x TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE sync_history (id INTEGER PRIMARY KEY, x TEXT)",
            [],
        )
        .unwrap();
        for t in [
            "sync_inbound",
            "sync_outbound",
            "sync_outbound_files",
            "sync_inbound_files",
            "sync_events",
            "sync_history",
        ] {
            conn.execute(&format!("INSERT INTO {t} DEFAULT VALUES"), [])
                .unwrap();
        }

        // Absent batch_uuid → wipe fires; no panic on the two missing tables.
        wipe_transfer_bookkeeping_on_upgrade(&conn).unwrap();
        for t in [
            "sync_inbound",
            "sync_outbound",
            "sync_outbound_files",
            "sync_inbound_files",
            "sync_events",
            "sync_history",
        ] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{t} wiped");
        }

        // Add the flag + a fresh row; a second wipe must NOT fire (idempotent flag).
        conn.execute("ALTER TABLE sync_inbound ADD COLUMN batch_uuid TEXT", [])
            .unwrap();
        conn.execute("INSERT INTO sync_outbound DEFAULT VALUES", [])
            .unwrap();
        wipe_transfer_bookkeeping_on_upgrade(&conn).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM sync_outbound", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "flag present → second wipe is a no-op");
    }

    /// Batch Model §Migration (review fix): a beta.1/beta.2 DB predates
    /// `sync_inbound` entirely — the table shipped only in beta.3 (Task 11) — so it
    /// carries `sync_outbound`/`sync_history` rows with NO `sync_inbound` table at
    /// all. The ORIGINAL detector (keyed solely on `sync_inbound.batch_uuid`) missed
    /// this shape once a caller's own `DDL_INBOUND` had already created a fresh,
    /// columned `sync_inbound` earlier in the same pass — it would read "column
    /// present" and skip. `transfer_wipe_needed` (via `wipe_transfer_bookkeeping_on_upgrade`)
    /// closes this: it fires when `sync_inbound` is absent but a legacy table
    /// (`sync_outbound`/`sync_history`) is present — pinned here at the raw-function
    /// level, independent of any caller's DDL ordering.
    #[test]
    fn upgrade_wipe_catches_beta1_shape_with_no_sync_inbound_table() {
        let conn = Connection::open_in_memory().unwrap();
        // The pre-Task-11 shape: sync_outbound / sync_history exist with rows;
        // sync_inbound does not exist AT ALL.
        conn.execute(
            "CREATE TABLE sync_outbound (id INTEGER PRIMARY KEY, x TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE sync_history (id INTEGER PRIMARY KEY, x TEXT)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO sync_outbound DEFAULT VALUES", [])
            .unwrap();
        conn.execute("INSERT INTO sync_history DEFAULT VALUES", [])
            .unwrap();
        assert!(
            !table_present(&conn, "sync_inbound").unwrap(),
            "sanity: no sync_inbound table (beta.1 shape)"
        );

        wipe_transfer_bookkeeping_on_upgrade(&conn).unwrap();

        let n_out: i64 = conn
            .query_row("SELECT COUNT(*) FROM sync_outbound", [], |r| r.get(0))
            .unwrap();
        let n_hist: i64 = conn
            .query_row("SELECT COUNT(*) FROM sync_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            n_out, 0,
            "sync_outbound wiped even though sync_inbound never existed"
        );
        assert_eq!(
            n_hist, 0,
            "sync_history wiped even though sync_inbound never existed"
        );

        // A truly empty DB (neither sync_inbound nor sync_outbound nor sync_history)
        // must NOT be flagged — guaranteed no-op, not skipped by flag-purity.
        let empty = Connection::open_in_memory().unwrap();
        assert!(
            !transfer_wipe_needed(&empty).unwrap(),
            "a genuinely fresh DB never needs a wipe"
        );
    }

    /// Batch Model §D1/§D3: `reset_outbound_for_resend` reuses the SAME row —
    /// `attempts += 1`, a fresh wire id, cleared error/retry/confirm fields, and
    /// every per-file row back to `pending`/`bytes_done 0`/cleared verdicts. A
    /// missing row errors.
    #[test]
    fn reset_outbound_for_resend_resets_row_and_files() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(DDL_OUTBOUND, []).unwrap();
        conn.execute(DDL_OUTBOUND_FILES, []).unwrap();

        // A confirmed row (attempts=1) with a stale error/retry + two per-file rows
        // in non-pending states carrying verdicts.
        let files = [
            AnnounceFileEntry {
                rel_path: "a.fits".into(),
                byte_size: 100,
                frame_uuid: "u-a".into(),
            },
            AnnounceFileEntry {
                rel_path: "b.fits".into(),
                byte_size: 200,
                frame_uuid: "u-b".into(),
            },
        ];
        let id = insert_outbound_with_files(
            &conn,
            "pkg",
            &node_id_hex(&PEER),
            Some("M42"),
            &files,
            PackageLayout::Batch,
        )
        .unwrap();
        conn.execute("UPDATE sync_outbound SET attempts = 1, wire_package_id = 'wire-old', confirmed_at = 't0', last_error = 'boom', next_retry_at = 't1', state = 'confirmed' WHERE id = ?1", params![id]).unwrap();
        set_outbound_file_state(
            &conn,
            id,
            "a.fits",
            OutboundFileState::Done,
            100,
            Some("ingested"),
            None,
        )
        .unwrap();
        set_outbound_file_state(
            &conn,
            id,
            "b.fits",
            OutboundFileState::Uploaded,
            200,
            None,
            Some("stale"),
        )
        .unwrap();

        // Enqueue → generation 1 (the DEFAULT); the confirmed row above never
        // resent, so it is still at generation 1 before this reset.
        assert_eq!(
            outbound_row_by_id(&conn, id).unwrap().unwrap().generation,
            1,
            "a never-resent row is at generation 1"
        );

        reset_outbound_for_resend(&conn, id, "wire-new").unwrap();

        let row = outbound_row_by_id(&conn, id).unwrap().unwrap();
        assert_eq!(row.state, OutboundState::Queued, "reset to queued");
        assert_eq!(row.attempts, 2, "attempts incremented");
        assert_eq!(
            row.generation, 2,
            "generation incremented (attempt N counter)"
        );
        assert_eq!(
            row.wire_package_id.as_deref(),
            Some("wire-new"),
            "fresh wire id"
        );
        assert_eq!(row.last_error, None, "last_error cleared");
        assert_eq!(row.next_retry_at, None, "next_retry_at cleared");
        assert_eq!(row.confirmed_at, None, "confirmed_at cleared");
        assert_eq!(
            row.display_name.as_deref(),
            Some("M42"),
            "display_name preserved"
        );

        for f in list_outbound_files(&conn, id).unwrap() {
            assert_eq!(
                f.state,
                OutboundFileState::Pending,
                "{} reset to pending",
                f.rel_path
            );
            assert_eq!(f.bytes_done, 0, "{} bytes_done reset", f.rel_path);
            assert_eq!(f.outcome, None, "{} outcome cleared", f.rel_path);
            assert_eq!(f.error, None, "{} error cleared", f.rel_path);
        }

        // A missing row errors, never silently succeeds.
        assert!(reset_outbound_for_resend(&conn, 9999, "wire-x").is_err());
    }

    /// Batch Model §D5: the engine's internal announce-retry (`bump_attempts`)
    /// advances the noisy `attempts` column but MUST NOT touch the user-facing
    /// `generation` counter — only a resend (`reset_outbound_for_resend`) bumps
    /// generation. Pins the double-bump confusion (`attempts` counts BOTH resends
    /// AND announce-retries, so it is unusable as "attempt N"; `generation` is the
    /// clean counter).
    #[test]
    fn announce_retry_bumps_attempts_but_not_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap();
        let id = store
            .enqueue("pkg", PEER, None, &[], PackageLayout::Batch)
            .unwrap();

        // Fresh enqueue → generation 1 (attempt 1), attempts 0.
        let row = store.get_outbound(id).unwrap().unwrap();
        assert_eq!(row.generation, 1, "enqueue starts at generation 1");
        assert_eq!(row.attempts, 0);

        // Three announce-retries advance `attempts` only.
        store.bump_attempts(id).unwrap();
        store.bump_attempts(id).unwrap();
        store.bump_attempts(id).unwrap();
        let row = store.get_outbound(id).unwrap().unwrap();
        assert_eq!(row.attempts, 3, "announce-retries advance attempts");
        assert_eq!(
            row.generation, 1,
            "announce-retries do NOT bump generation (still attempt 1)"
        );

        // A resend advances the generation (to attempt 2), even though attempts is
        // already ahead — the two counters are independent.
        store.reset_outbound_for_resend(id, "wire-2").unwrap();
        let row = store.get_outbound(id).unwrap().unwrap();
        assert_eq!(row.generation, 2, "a resend bumps generation to attempt 2");
        assert_eq!(row.attempts, 4, "the reset also bumps attempts");
    }

    /// Batch Model §D1 + Decline Finality Axis §D3: `upsert_inbound_attempt`
    /// inserts on first announce, resets an existing row to `announced` with the
    /// new wire id while PRESERVING `display_name`/`landing_dir` — including a
    /// sender-revoked `cancelled` row (the smoke-№7 fix) — and leaves a DECLINED
    /// row (`declined_at` non-NULL) untouched with the `declined_final` flag,
    /// regardless of its current attempt-level state. `get_inbound_by_batch`
    /// resolves the row; the unique `(peer, batch_uuid)` index collapses two
    /// attempts of one batch into one row while keeping two distinct batches apart.
    #[test]
    fn upsert_inbound_attempt_batch_keyed_lifecycle() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(DDL_INBOUND, []).unwrap();
        conn.execute(
            "CREATE UNIQUE INDEX idx_sync_inbound_batch ON sync_inbound(peer, batch_uuid)",
            [],
        )
        .unwrap();
        let peer = node_id_hex(&PEER);

        // First announce → INSERT, not cancelled-final.
        let (id, cancelled) =
            upsert_inbound_attempt(&conn, &peer, "batch-1", "wire-1", 3, 300).unwrap();
        assert!(!cancelled);
        let row = get_inbound_by_batch(&conn, &peer, "batch-1")
            .unwrap()
            .unwrap();
        assert_eq!(row.id, id);
        assert_eq!(row.state, InboundState::Announced);
        assert_eq!(row.package_id, "wire-1", "wire id stamped");
        assert_eq!(row.batch_uuid.as_deref(), Some("batch-1"));
        assert_eq!(row.frame_count, 3);
        assert_eq!(row.generation, 1, "first announce → generation 1");

        // Give the row a name + landing dir + advance it, then a SECOND attempt.
        set_inbound_display_name(&conn, id, Some("Downloads M42")).unwrap();
        set_inbound_landing_dir(&conn, id, "/incoming/peer/m42").unwrap();
        set_inbound_state(&conn, "wire-1", InboundState::Fetching, None).unwrap();
        set_inbound_bytes_done(&conn, "wire-1", 150).unwrap();

        let (id2, cancelled2) =
            upsert_inbound_attempt(&conn, &peer, "batch-1", "wire-2", 4, 400).unwrap();
        assert!(!cancelled2);
        assert_eq!(
            id2, id,
            "same batch → same row (one row per (peer, batch_uuid))"
        );
        let row = get_inbound_by_batch(&conn, &peer, "batch-1")
            .unwrap()
            .unwrap();
        assert_eq!(row.state, InboundState::Announced, "reset to announced");
        assert_eq!(row.package_id, "wire-2", "new attempt's wire id");
        assert_eq!(row.bytes_done, 0, "bytes_done reset");
        assert_eq!(row.frame_count, 4, "counts refreshed");
        assert_eq!(row.generation, 2, "a re-attempt upsert bumps generation");
        assert_eq!(
            row.display_name.as_deref(),
            Some("Downloads M42"),
            "display_name preserved"
        );
        assert_eq!(
            row.landing_dir.as_deref(),
            Some("/incoming/peer/m42"),
            "landing_dir preserved"
        );

        // A second, distinct batch from the same peer → a distinct row (index allows it).
        let (id3, _) = upsert_inbound_attempt(&conn, &peer, "batch-2", "wire-3", 1, 10).unwrap();
        assert_ne!(id3, id, "distinct batch_uuid → distinct row");

        // Decline Finality Axis §D3: a SENDER-revoked cancelled row (declined_at
        // NULL) is NOT final — the sender's resend resets it like any other
        // attempt terminal (the smoke-№7 fix).
        set_inbound_state(&conn, "wire-2", InboundState::Cancelled, Some("by sender")).unwrap();
        let (id4, final4) =
            upsert_inbound_attempt(&conn, &peer, "batch-1", "wire-4", 9, 900).unwrap();
        assert!(
            !final4,
            "a revoke-cancelled row (no declined_at) is NOT declined-final"
        );
        assert_eq!(id4, id, "same row id returned");
        let row = get_inbound_by_batch(&conn, &peer, "batch-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            row.state,
            InboundState::Announced,
            "the resend reset the revoked row"
        );
        assert_eq!(row.package_id, "wire-4", "the resend's wire id was stamped");
        assert_eq!(row.generation, 3, "the resend bumped the generation");
        assert!(row.declined_at.is_none(), "a reset never invents a decline");

        // Declined-is-final: once declined_at is stamped the row refuses every
        // re-announce untouched…
        set_inbound_state(&conn, "wire-4", InboundState::Cancelled, None).unwrap();
        set_inbound_declined_at(&conn, id).unwrap();
        let declined_stamp = get_inbound_by_batch(&conn, &peer, "batch-1")
            .unwrap()
            .unwrap()
            .declined_at;
        assert!(declined_stamp.is_some(), "declined_at stamped");
        let (id5, final5) =
            upsert_inbound_attempt(&conn, &peer, "batch-1", "wire-5", 9, 900).unwrap();
        assert!(final5, "a declined row reports declined_final");
        assert_eq!(id5, id, "same row id returned");
        let row = get_inbound_by_batch(&conn, &peer, "batch-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            row.state,
            InboundState::Cancelled,
            "still cancelled — untouched"
        );
        assert_eq!(
            row.package_id, "wire-4",
            "wire id NOT rotated by the upsert on a declined row"
        );
        assert_eq!(
            row.generation, 3,
            "generation NOT bumped on a declined-final row"
        );
        assert_eq!(
            row.declined_at, declined_stamp,
            "the decline stamp is first-write-wins"
        );
        // …and re-stamping is a no-op (first-write-wins).
        set_inbound_declined_at(&conn, id).unwrap();
        let row = get_inbound_by_batch(&conn, &peer, "batch-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            row.declined_at, declined_stamp,
            "re-stamp preserves the original instant"
        );

        // …EVEN when the attempt-level state was later overwritten (a restart
        // reconcile stamps `failed "interrupted by restart"`): the decline survives.
        set_inbound_state(
            &conn,
            "wire-4",
            InboundState::Failed,
            Some("interrupted by restart"),
        )
        .unwrap();
        let (id6, final6) =
            upsert_inbound_attempt(&conn, &peer, "batch-1", "wire-6", 9, 900).unwrap();
        assert!(
            final6,
            "a declined row stays final across a state overwrite"
        );
        assert_eq!(id6, id, "same row id returned");
        let row = get_inbound_by_batch(&conn, &peer, "batch-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            row.state,
            InboundState::Failed,
            "untouched — the reconciled state stands"
        );

        // A lookup for an unknown batch is None, never an error.
        assert!(get_inbound_by_batch(&conn, &peer, "nope")
            .unwrap()
            .is_none());
    }

    /// Decline Finality Axis §D1: adding `declined_at` to a pre-axis table
    /// backfills every receiver-declined `cancelled` row (the only `cancelled`
    /// rows with `last_error = 'by sender'` are sender revokes — those must NOT
    /// become declines), and the migration is idempotent.
    #[test]
    fn declined_at_migration_backfills_receiver_declines() {
        let conn = Connection::open_in_memory().unwrap();
        // Pre-axis table shape: PROVABLY "current DDL minus `declined_at`" —
        // derived from the canonical constant so a future column add can never
        // silently stale this fixture.
        conn.execute(DDL_INBOUND, []).unwrap();
        conn.execute("ALTER TABLE sync_inbound DROP COLUMN declined_at", [])
            .unwrap();
        let peer = node_id_hex(&PEER);
        let insert = |pkg: &str, state: &str, err: Option<&str>, finished: Option<&str>| {
            conn.execute(
                "INSERT INTO sync_inbound (peer, package_id, batch_uuid, state, last_error, created_at, finished_at)
                 VALUES (?1, ?2, ?2, ?3, ?4, '2026-07-22T08:00:00Z', ?5)",
                params![peer, pkg, state, err, finished],
            )
            .unwrap();
        };
        // Pin the load-bearing literal: the backfill excludes exactly the detail
        // `handle_revoke` writes — an accidental edit of either side must fail here.
        assert_eq!(crate::sync::models::REVOKED_BY_SENDER_DETAIL, "by sender");
        insert(
            "declined-null-err",
            "cancelled",
            None,
            Some("2026-07-22T08:40:00Z"),
        );
        insert("declined-detail", "cancelled", Some("declined"), None);
        insert(
            "revoked-by-sender",
            "cancelled",
            Some("by sender"),
            Some("2026-07-22T08:41:00Z"),
        );
        insert(
            "plain-failed",
            "failed",
            Some("sender failed"),
            Some("2026-07-22T08:42:00Z"),
        );
        insert("live-fetching", "fetching", None, None);

        ensure_inbound_columns(&conn).unwrap();

        let declined_of = |pkg: &str| -> Option<String> {
            conn.query_row(
                "SELECT declined_at FROM sync_inbound WHERE package_id = ?1",
                params![pkg],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            declined_of("declined-null-err").as_deref(),
            Some("2026-07-22T08:40:00Z"),
            "a NULL-error cancelled row backfills declined_at from finished_at"
        );
        assert_eq!(
            declined_of("declined-detail").as_deref(),
            Some("2026-07-22T08:00:00Z"),
            "a cancelled row without finished_at falls back to created_at"
        );
        assert!(
            declined_of("revoked-by-sender").is_none(),
            "a sender revoke never becomes a decline"
        );
        assert!(
            declined_of("plain-failed").is_none(),
            "failed rows are untouched"
        );
        assert!(
            declined_of("live-fetching").is_none(),
            "live rows are untouched"
        );

        // Idempotent: a second pass (column now present) changes nothing.
        ensure_inbound_columns(&conn).unwrap();
        assert!(
            declined_of("revoked-by-sender").is_none(),
            "re-run stays a no-op"
        );
        assert_eq!(
            declined_of("declined-null-err").as_deref(),
            Some("2026-07-22T08:40:00Z")
        );
    }

    /// Perseus UI v2 (Task 2): `delete_outbound_group` history-deletes a whole
    /// fan-out batch's sender bookkeeping — each row, its per-file rows, and its
    /// `sync_events` journal — in ONE transaction, leaving nothing behind. Seeded
    /// only through the store's own trait/public API (`enqueue` +
    /// `replace_outbound_files` + `append_sync_event`).
    #[test]
    fn delete_outbound_group_removes_rows_files_and_journals_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StandaloneSyncStore::open(tmp.path().join("s.db")).unwrap();

        // Two rows of one fan-out batch: same package_ref, two distinct peers.
        let peer2: NodeId = [9u8; 32];
        let id1 = store
            .enqueue("pkg-fanout", PEER, Some("M42"), &[], PackageLayout::Batch)
            .unwrap();
        let id2 = store
            .enqueue("pkg-fanout", peer2, Some("M42"), &[], PackageLayout::Batch)
            .unwrap();

        // Two per-file rows on each row (the row id is the parent key).
        let files_for = |id: i64| {
            vec![
                OutboundFileRow {
                    outbound_id: id,
                    rel_path: "a.fits".into(),
                    byte_size: 100,
                    frame_uuid: "u-a".into(),
                    state: OutboundFileState::Pending,
                    bytes_done: 0,
                    outcome: None,
                    error: None,
                    updated_at: now_iso(),
                },
                OutboundFileRow {
                    outbound_id: id,
                    rel_path: "b.fits".into(),
                    byte_size: 200,
                    frame_uuid: "u-b".into(),
                    state: OutboundFileState::Pending,
                    bytes_done: 0,
                    outcome: None,
                    error: None,
                    updated_at: now_iso(),
                },
            ]
        };
        store.replace_outbound_files(id1, &files_for(id1)).unwrap();
        store.replace_outbound_files(id2, &files_for(id2)).unwrap();

        // …and one journal event each — `batch_key` is the row id as text (the
        // engine's own journaling convention, mirrored by `list_sync_events_for`).
        store
            .append_sync_event(Direction::Sent, &id1.to_string(), "enqueued", None)
            .unwrap();
        store
            .append_sync_event(Direction::Sent, &id2.to_string(), "enqueued", None)
            .unwrap();

        // Sanity: everything is present before the delete.
        assert_eq!(store.list_outbound_files(id1).unwrap().len(), 2);
        assert_eq!(store.list_sync_events_for(id1).unwrap().len(), 1);

        store.delete_outbound_group(&[id1, id2]).unwrap();

        // Both rows, their per-file rows, and their journals are gone.
        assert!(store.get_outbound(id1).unwrap().is_none());
        assert!(store.get_outbound(id2).unwrap().is_none());
        assert!(store.list_outbound_files(id1).unwrap().is_empty());
        assert!(store.list_outbound_files(id2).unwrap().is_empty());
        assert!(store.list_sync_events_for(id1).unwrap().is_empty());
        assert!(store.list_sync_events_for(id2).unwrap().is_empty());
    }

    /// Perseus UI v2 (Task 2): `list_sync_events_for` reads exactly one outbound
    /// row's `Sent` journal (keyed by the row id as text), newest-first per
    /// [`list_sync_events`], with direction/kind/detail preserved and scoped to
    /// that single row.
    #[test]
    fn list_sync_events_for_reads_the_sent_journal_of_one_row() {
        let tmp = tempfile::tempdir().unwrap();
        let store = StandaloneSyncStore::open(tmp.path().join("s.db")).unwrap();
        let id = store
            .enqueue("pkg", PEER, None, &[], PackageLayout::Batch)
            .unwrap();

        store
            .append_sync_event(Direction::Sent, &id.to_string(), "enqueued", None)
            .unwrap();
        store
            .append_sync_event(
                Direction::Sent,
                &id.to_string(),
                "announce_sent",
                Some("relay"),
            )
            .unwrap();

        let events = store.list_sync_events_for(id).unwrap();
        assert_eq!(
            events.len(),
            2,
            "both events of this row's journal come back"
        );
        // `list_sync_events` is newest-first (id DESC), so the later insert leads.
        assert_eq!(events[0].kind, "announce_sent");
        assert_eq!(events[0].detail.as_deref(), Some("relay"));
        assert_eq!(events[1].kind, "enqueued");
        assert_eq!(events[1].detail, None);
        assert!(
            events.iter().all(|e| e.direction == Direction::Sent),
            "the Sent journal"
        );

        // A different row's journal is never mixed in (scoped to one batch_key).
        let other = store
            .enqueue("pkg2", PEER, None, &[], PackageLayout::Batch)
            .unwrap();
        store
            .append_sync_event(Direction::Sent, &other.to_string(), "enqueued", None)
            .unwrap();
        assert_eq!(
            store.list_sync_events_for(id).unwrap().len(),
            2,
            "still only this row's two events"
        );
        assert_eq!(store.list_sync_events_for(other).unwrap().len(), 1);
    }

    /// D2 §3.1/§4: `Waiting` is a NON-terminal end-of-attempt, and every
    /// membership test must answer for it. The three terminal-state lists are
    /// hand-duplicated SQL literals rather than derivations of `is_terminal()`, so
    /// "a new non-terminal state is admitted automatically" is correct only by
    /// duplication — this test is what makes that claim checkable.
    #[test]
    fn waiting_is_non_terminal_and_lands_in_every_active_membership_test() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(DDL_INBOUND, []).unwrap();
        conn.execute(
            "CREATE UNIQUE INDEX idx_sync_inbound_batch ON sync_inbound(peer, batch_uuid)",
            [],
        )
        .unwrap();
        let peer = node_id_hex(&PEER);

        assert!(!InboundState::Waiting.is_terminal());
        assert_eq!(InboundState::Waiting.as_str(), "waiting");
        assert_eq!(
            InboundState::from_db("waiting").unwrap(),
            InboundState::Waiting
        );

        let (id, _) = upsert_inbound_attempt(&conn, &peer, "batch-1", "wire-1", 3, 999).unwrap();
        set_inbound_state(&conn, "wire-1", InboundState::Waiting, Some("peer gone")).unwrap();

        // Active, not terminal, and still holding its landing-dir claim.
        let active = inbound_active(&conn).unwrap();
        assert_eq!(active.len(), 1, "a waiting row is an active row");
        assert_eq!(active[0].state, InboundState::Waiting);
        assert!(
            active[0].finished_at.is_none(),
            "a non-terminal state stamps no finished_at"
        );
        assert_eq!(
            active[0].last_error.as_deref(),
            Some("peer gone"),
            "the reason is preserved"
        );

        assert!(
            terminal_inbound(&conn, 50).unwrap().is_empty(),
            "and absent from the terminal window"
        );

        set_inbound_landing_dir(&conn, id, "/incoming/dev/batch").unwrap();
        assert!(
            landing_dir_claimed_by_active(&conn, "/incoming/dev/batch", id + 1).unwrap(),
            "a waiting row keeps claiming its landing dir — the sender redelivers into the same tree"
        );

        // A re-announce revives it into a fresh attempt (no state list to teach).
        let (again, declined) =
            upsert_inbound_attempt(&conn, &peer, "batch-1", "wire-2", 3, 999).unwrap();
        assert_eq!(again, id, "same durable row");
        assert!(!declined);
        let revived = get_inbound(&conn, "wire-2").unwrap().unwrap();
        assert_eq!(revived.state, InboundState::Announced);
        assert_eq!(revived.generation, 2);
        assert!(revived.last_error.is_none(), "the revive clears the reason");
    }
}
