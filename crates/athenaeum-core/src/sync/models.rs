//! Persisted row types for the sync engine and their text encodings.
//!
//! These structs mirror the `sync_outbound` / `sync_history` tables (DDL in
//! [`super::store`]). The engine and store map them to/from SQLite columns; the
//! serde derives (camelCase) exist for the later IPC surface (task M3) — the DB
//! encoding goes through the explicit [`OutboundState::as_str`] /
//! [`OutboundState::from_db`] helpers, not serde.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::sharing::types::NodeId;

/// Sender-side lifecycle of one outbound package.
///
/// Collapsed for v1 (see the [module docs](super)): `Delivered` is retained in
/// the enum — keeping the DDL `state TEXT` value space stable for later tasks —
/// but is never written by the engine, which learns completion from the peer's
/// ack. Terminal states are [`Confirmed`](Self::Confirmed),
/// [`Failed`](Self::Failed) and [`Cancelled`](Self::Cancelled); everything else
/// is non-terminal and re-driven on crash-resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum OutboundState {
    /// Persisted, not yet advertised to the peer.
    Queued,
    /// Announced to the peer; awaiting pull.
    Announced,
    /// Peer is (or should be) pulling the package; awaiting the ack.
    Transferring,
    /// Reserved (v1-unused): a distinct "peer finished pulling, not yet acked"
    /// signal that loopback/iroh do not surface to the sender.
    Delivered,
    /// Peer acked receipt — terminal success. **Requires every frame's receipt
    /// to be non-`Rejected`** (task A7 fix-review): `Confirmed` means "all
    /// frames ingested-or-duplicate". An ack carrying any `Rejected` receipt is
    /// a partial delivery — the engine does NOT confirm; the package stays in
    /// flight so the normal ack-timeout/retry path re-announces (redelivery)
    /// forever until the peer accepts every frame. Pending states never expire
    /// from a timeout (delivery-forever cycle, Tasks 1–2).
    Confirmed,
    /// Terminal failure reserved for a **local, unrecoverable** payload problem
    /// (e.g. the package dir is gone — see the missing-payload guard) or a
    /// legacy row written before the delivery-forever cycle. It is NOT reached
    /// from ack timeouts or from partial/rejected acks: those re-deliver forever
    /// (`Confirmed` doc) rather than exhausting to `Failed`.
    Failed,
    /// User cancel — terminal. Reached when the sender explicitly cancels an
    /// in-flight package, or (a later task) when the receiver acks every frame
    /// as cancelled. Distinct from [`Failed`](Self::Failed) so the UI and retry
    /// eligibility can tell "user gave up" apart from "payload broke".
    Cancelled,
}

impl OutboundState {
    /// Stable lowercase text stored in the `state` column.
    pub fn as_str(&self) -> &'static str {
        match self {
            OutboundState::Queued => "queued",
            OutboundState::Announced => "announced",
            OutboundState::Transferring => "transferring",
            OutboundState::Delivered => "delivered",
            OutboundState::Confirmed => "confirmed",
            OutboundState::Failed => "failed",
            OutboundState::Cancelled => "cancelled",
        }
    }

    /// Parse the `state` column text. Errors on an unknown value rather than
    /// silently coercing.
    pub fn from_db(s: &str) -> Result<Self> {
        Ok(match s {
            "queued" => OutboundState::Queued,
            "announced" => OutboundState::Announced,
            "transferring" => OutboundState::Transferring,
            "delivered" => OutboundState::Delivered,
            "confirmed" => OutboundState::Confirmed,
            "failed" => OutboundState::Failed,
            "cancelled" => OutboundState::Cancelled,
            other => return Err(anyhow!("unknown outbound state: {other}")),
        })
    }

    /// True for the three terminal states ([`Confirmed`](Self::Confirmed),
    /// [`Failed`](Self::Failed), [`Cancelled`](Self::Cancelled)) that
    /// crash-resume must *not* re-drive.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            OutboundState::Confirmed | OutboundState::Failed | OutboundState::Cancelled
        )
    }
}

/// Receiver-side lifecycle of one inbound package (`sync_inbound`, Task 11).
///
/// Mirrors [`OutboundState`]'s `as_str`/`from_db`/`is_terminal` shape. The
/// receiver walks a package `Announced → Fetching → Ingesting → Done` and stamps
/// `Failed` (with a `last_error`) when a fetch fails or every frame is rejected.
/// [`Cancelled`](Self::Cancelled) is the receiver-decline terminal (Task 12) and
/// is **final**: a re-announce never resurrects a cancelled row. Terminal states
/// ([`Done`](Self::Done), [`Failed`](Self::Failed), [`Cancelled`](Self::Cancelled))
/// are excluded from [`inbound_active`](super::store::inbound_active).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum InboundState {
    /// Advertised to us; not yet fetched.
    Announced,
    /// Pulling the package blobs (live `bytes_done` updates during this stage).
    Fetching,
    /// Fetched; landing + cataloguing the frames.
    Ingesting,
    /// Terminal success: all frames ingested (or a partial ingest with some
    /// rejections), or re-acked from the receipt log.
    Done,
    /// Terminal failure: the fetch failed, or every frame was rejected.
    Failed,
    /// Terminal receiver-decline (Task 12). Final — a re-announce leaves the row
    /// untouched (the caller checks state before fetching).
    Cancelled,
}

impl InboundState {
    /// Stable lowercase text stored in the `state` column.
    pub fn as_str(&self) -> &'static str {
        match self {
            InboundState::Announced => "announced",
            InboundState::Fetching => "fetching",
            InboundState::Ingesting => "ingesting",
            InboundState::Done => "done",
            InboundState::Failed => "failed",
            InboundState::Cancelled => "cancelled",
        }
    }

    /// Parse the `state` column text. Errors on an unknown value rather than
    /// silently coercing.
    pub fn from_db(s: &str) -> Result<Self> {
        Ok(match s {
            "announced" => InboundState::Announced,
            "fetching" => InboundState::Fetching,
            "ingesting" => InboundState::Ingesting,
            "done" => InboundState::Done,
            "failed" => InboundState::Failed,
            "cancelled" => InboundState::Cancelled,
            other => return Err(anyhow!("unknown inbound state: {other}")),
        })
    }

    /// True for the three terminal states ([`Done`](Self::Done),
    /// [`Failed`](Self::Failed), [`Cancelled`](Self::Cancelled)) that
    /// [`inbound_active`](super::store::inbound_active) excludes and that
    /// [`set_inbound_state`](super::store::set_inbound_state) stamps `finished_at`
    /// for.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            InboundState::Done | InboundState::Failed | InboundState::Cancelled
        )
    }
}

/// One row of `sync_inbound`: the durable state of a package we are receiving
/// (Task 11). Keyed uniquely on `(peer, package_id)`; `peer` is the sending
/// node id (hex). `bytes_done` is refreshed live during the fetch stage from the
/// transport's [`FetchEvent::Batch`](crate::sharing::types::FetchEvent) ticks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboundRow {
    pub id: i64,
    /// Sending peer's node id, hex-encoded.
    pub peer: String,
    pub package_id: String,
    pub state: InboundState,
    pub frame_count: u32,
    pub byte_size: u64,
    /// Cumulative bytes fetched so far (0 until the fetch stage reports progress).
    pub bytes_done: u64,
    /// The failure reason when `state` is [`Failed`](InboundState::Failed), else
    /// `None`.
    pub last_error: Option<String>,
    pub created_at: String,
    /// Stamped when the row reaches a terminal state, else `None`.
    pub finished_at: Option<String>,
    /// Human batch name carried in the v2 announce manifest (Transfers Status
    /// Model v2, §D1). `None` for a v1 announce (no name on the wire) or a legacy
    /// row created before this column. The receiver shows this instead of the raw
    /// `package_id`; written by the receiver at announce time.
    #[serde(default)]
    pub display_name: Option<String>,
    /// The resolved on-disk landing directory for this package's files (Transfers
    /// Status Model v2, §D2): `<incoming_root>/<sender_slug>/<batch_slug>` for a
    /// named (v2) batch, collision-suffixed `_2`/`_3`… A v1/unnamed batch never
    /// resolves one (the receiver lands under `<incoming_root>/<sender_slug>`
    /// directly, computed at ingest time), so it stays `None`. Resolved ONCE per
    /// package at first landing and reused on resume so a restart lands into the
    /// same tree; written by the receiver.
    #[serde(default)]
    pub landing_dir: Option<String>,
    /// The durable per-transfer batch identity (Transfers Batch Model, §D1). The
    /// receiver's row is keyed `(peer, batch_uuid)` — ONE long-lived row across
    /// every attempt of one transfer — enforced by the `idx_sync_inbound_batch`
    /// unique index. Carried in a v3 announce; a v1/v2 (legacy) announce has no
    /// batch identity on the wire, so the receiver falls back to `batch_uuid :=`
    /// wire package id (B4), and a row created before this column stays `None`.
    /// New rows always set it via
    /// [`upsert_inbound_attempt`](super::store::upsert_inbound_attempt).
    #[serde(default)]
    pub batch_uuid: Option<String>,
    /// Collab forward-compat key (Transfers Batch Model, §D6): the `project_id`
    /// this transfer moves for, distinguishing a collab-swarm transfer from a
    /// personal one and giving the future planner its key. `None` for a personal
    /// sync (no project dimension).
    #[serde(default)]
    pub project_id: Option<String>,
    /// User-facing transfer generation counter (Transfers Batch Model §D5): the
    /// "attempt N" the UI shows. Starts at `1` on the first announce and is bumped
    /// ONLY by a receiver re-attempt ([`upsert_inbound_attempt`](super::store::upsert_inbound_attempt)'s
    /// existing-row reset branch) — NOT by the engine's internal announce-retries
    /// (which bump the noisy `attempts` column). Distinct from `attempts` so the UI
    /// has a clean "attempt N" that ignores announce churn.
    #[serde(default)]
    pub generation: u32,
}

/// Direction of a transfer recorded in [`HistoryRow`]. This sender-side task
/// only ever writes [`Sent`](Self::Sent); [`Received`](Self::Received) is for
/// the receiver task (A7) writing into the same table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    Sent,
    Received,
}

impl Direction {
    /// Stable lowercase text stored in the `direction` column.
    pub fn as_str(&self) -> &'static str {
        match self {
            Direction::Sent => "sent",
            Direction::Received => "received",
        }
    }

    /// Parse the `direction` column text.
    pub fn from_db(s: &str) -> Result<Self> {
        Ok(match s {
            "sent" => Direction::Sent,
            "received" => Direction::Received,
            other => return Err(anyhow!("unknown direction: {other}")),
        })
    }
}

/// One row of `sync_outbound`: the durable state of a package we are sending.
///
/// `package_ref` is the on-disk package directory (task A3 layout); the engine
/// re-reads its manifest to (re-)advertise and to build history. `peer` is the
/// destination node id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboundRow {
    pub id: i64,
    /// Package directory path.
    pub package_ref: String,
    pub peer: NodeId,
    pub state: OutboundState,
    pub attempts: u32,
    pub created_at: String,
    pub confirmed_at: Option<String>,
    /// The most recent failed-attempt reason (Task 9), or `None` when the package
    /// has never failed / was cleared on success or confirm. Written by the engine
    /// on each failed serve/announce attempt and each ack-timeout, cleared when the
    /// package announces successfully or reaches [`Confirmed`](OutboundState::Confirmed);
    /// surfaced beside `attempts` on the Perseus status page so an operator sees
    /// *why* a package is stuck or failed, not just that it retried.
    #[serde(default)]
    pub last_error: Option<String>,
    /// The wall-clock deadline (RFC3339 UTC, formatted like `created_at`) of the
    /// next scheduled retry attempt (Task 2), or `None` when the package is not
    /// currently waiting out a backoff window — it is awaiting an ack or terminal.
    /// Written by the engine every time it arms a `Retry` deadline and cleared on
    /// a successful announce / terminal transition, so the Transfers UI can render
    /// a live countdown and a restart re-arms honestly. Read by tasks 9/14.
    #[serde(default)]
    pub next_retry_at: Option<String>,
    /// The stable wire `package_id` (a v4 UUID string) minted for this row's
    /// announce (zombie-inbound fix). `None` until the engine's first successful
    /// announce build persists it; from then on every crash-resume re-announce
    /// reuses it, so the wire id is stable across sender restarts and the
    /// receiver's `sync_inbound` row (keyed on the wire id) is reused rather than
    /// orphaned. A re-enqueue (retry) is a fresh row that starts `None` and mints
    /// a new id — deliberate, so a resend transfers anew instead of replay-acking
    /// from stale receiver receipts.
    #[serde(default)]
    pub wire_package_id: Option<String>,
    /// Human batch name for this send (Transfers Status Model v2, §D1) — an
    /// auto-derived-then-editable label (frame-set name / common root dir / `N
    /// files — <ts>`) that travels in the v2 announce manifest so both sides show
    /// the same name. `None` for a legacy row created before this column; the send
    /// path fills it. The raw UUIDs appear only in the Details tab.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Collab forward-compat key (Transfers Batch Model, §D6): the `project_id`
    /// this transfer moves for, distinguishing a collab-swarm transfer from a
    /// personal one and giving the future planner its key. `None` for a personal
    /// sync; the collab send path (a later wave) fills it.
    #[serde(default)]
    pub project_id: Option<String>,
    /// User-facing transfer generation counter (Transfers Batch Model §D5): the
    /// "attempt N" the UI shows. Starts at `1` at enqueue and is bumped ONLY by
    /// [`reset_outbound_for_resend`](super::store::reset_outbound_for_resend) (a
    /// user-driven resend) — NOT by the engine's internal announce-retries (which
    /// bump the noisy `attempts` column). Distinct from `attempts` so the UI has a
    /// clean "attempt N" that ignores announce churn.
    #[serde(default)]
    pub generation: u32,
}

/// One row of `sync_history`: an append-only audit entry for a per-frame
/// transfer event (transfer started, confirmed, failed, or cancelled).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRow {
    pub frame_uuid: String,
    pub filename: String,
    pub object: Option<String>,
    /// Peer node id, hex-encoded.
    pub peer_device: String,
    pub direction: Direction,
    pub bytes: u64,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// Short outcome tag: `sent`, `ingested`, `duplicate`, `rejected`, `failed`
    /// (or the composite `failed: rejected frame(s) <uuids>`), or `cancelled`.
    /// Retention / manual-deletion audit rows reuse this column too:
    /// `retention_deleted` (an automatic retention pass) and `deleted_manual`
    /// (the web "Delete selected" action).
    pub outcome: String,
    /// Stage-II collab provenance: the `project_id` this frame moved for, taken
    /// from the manifest record's [`ProjectStamp`](crate::package::ProjectStamp).
    /// `None` for a personal-sync transfer (no project dimension) — the Transfers
    /// UI shows a project chip only for the collab rows.
    #[serde(default)]
    pub project: Option<String>,
    /// The per-package batch key this frame-transfer belonged to (Task 14) — the
    /// durable handle each side already knows: the SENDER stamps its package
    /// dir's basename (== the last component of `sync_outbound.package_ref`), the
    /// RECEIVER stamps the wire announce `package_id` (== `sync_inbound.package_id`).
    /// [`list_transfer_files`](crate::api::sync::list_transfer_files) recovers the
    /// same key from the outbound / inbound row to join a package's per-frame
    /// verdicts back to its manifest. Legacy rows written before this column stay
    /// `None`.
    #[serde(default)]
    pub package_id: Option<String>,
    /// The human batch name this transfer belonged to (Transfers Status Model v2,
    /// §D1) — mirrors [`OutboundRow::display_name`] / [`InboundRow::display_name`]
    /// so the transfer log can show the named batch without re-joining the live
    /// row. `None` for legacy rows and for callers that do not yet supply it
    /// (later tasks fill it).
    #[serde(default)]
    pub batch_name: Option<String>,
}

/// Minimal query surface for [`SyncStore::search_history`](super::store::SyncStore::search_history).
///
/// Exact-match filters on `filename`, `object`, `direction`, and/or `peer_device`
/// (all optional; `None` = unfiltered), newest-first, capped at `limit`. Every
/// filter is applied SQL-side in [`search_history_rows`](super::store::search_history_rows)
/// (task M3 added `direction`/`peer` so the Transfers UI can split the log by
/// send/receive and by peer without post-filtering in the client).
#[derive(Debug, Clone, Default)]
pub struct HistoryQuery {
    pub filename: Option<String>,
    pub object: Option<String>,
    /// Restrict to one transfer direction (`Sent` / `Received`).
    pub direction: Option<Direction>,
    /// Exact peer node id (hex) to restrict to.
    pub peer: Option<String>,
    /// Exact `project_id` filter (unfiltered when absent) — the Transfers UI's
    /// project-dimension passthrough (Stage II collab, Task 11).
    pub project: Option<String>,
    /// Exact per-package batch-key filter (unfiltered when absent) — Task 14's
    /// per-batch detail read ([`list_transfer_files`](crate::api::sync::list_transfer_files))
    /// scopes a package's per-frame rows by [`HistoryRow::package_id`].
    pub package_id: Option<String>,
    pub limit: u32,
}

// ── Transfers Status Model v2: per-file rows + event-log rows ────────────────
//
// The DB-side cousins of the wire manifest ([`AnnounceFileEntry`](crate::sharing::types::AnnounceFileEntry)):
// one persisted row per file per batch on each side, so per-file status survives
// restart/resume (spec §D4), plus a capped per-batch event journal (§D7). DDL +
// CRUD live in [`super::store`]; text encodings go through the explicit
// `as_str`/`from_db` helpers, not serde.

/// Sender-side lifecycle of one file within an outbound batch (`sync_outbound_files`,
/// §D4). Mirrors [`OutboundState`]'s `as_str`/`from_db` shape: a file walks
/// `Pending → Sending → Uploaded → Done`. `Uploaded` means "the sender finished
/// serving this file's bytes"; `Done` means the batch ack recorded the receiver's
/// per-frame verdict for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum OutboundFileState {
    /// Persisted at build time; not yet served.
    Pending,
    /// Bytes are being served to the peer (live `bytes_done` updates here).
    Sending,
    /// The sender finished serving this file's bytes; awaiting the batch ack.
    Uploaded,
    /// The batch ack recorded this file's per-frame receipt — terminal.
    Done,
}

impl OutboundFileState {
    /// Stable lowercase text stored in the `state` column.
    pub fn as_str(&self) -> &'static str {
        match self {
            OutboundFileState::Pending => "pending",
            OutboundFileState::Sending => "sending",
            OutboundFileState::Uploaded => "uploaded",
            OutboundFileState::Done => "done",
        }
    }

    /// Parse the `state` column text. Errors on an unknown value rather than
    /// silently coercing.
    pub fn from_db(s: &str) -> Result<Self> {
        Ok(match s {
            "pending" => OutboundFileState::Pending,
            "sending" => OutboundFileState::Sending,
            "uploaded" => OutboundFileState::Uploaded,
            "done" => OutboundFileState::Done,
            other => return Err(anyhow!("unknown outbound file state: {other}")),
        })
    }
}

/// Receiver-side lifecycle of one file within an inbound batch (`sync_inbound_files`,
/// §D4). Mirrors [`InboundState`]'s `as_str`/`from_db` shape: a file walks
/// `Announced → Fetching → Done`, or lands `Failed` when its fetch/ingest fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum InboundFileState {
    /// Advertised in the announce manifest; not yet fetched.
    Announced,
    /// Pulling this file's bytes (live `bytes_done` updates here).
    Fetching,
    /// Fetched + ingested — terminal success.
    Done,
    /// The fetch or ingest of this file failed — terminal failure (the batch as a
    /// whole never fails; a per-file failure is recorded here).
    Failed,
}

impl InboundFileState {
    /// Stable lowercase text stored in the `state` column.
    pub fn as_str(&self) -> &'static str {
        match self {
            InboundFileState::Announced => "announced",
            InboundFileState::Fetching => "fetching",
            InboundFileState::Done => "done",
            InboundFileState::Failed => "failed",
        }
    }

    /// Parse the `state` column text. Errors on an unknown value rather than
    /// silently coercing.
    pub fn from_db(s: &str) -> Result<Self> {
        Ok(match s {
            "announced" => InboundFileState::Announced,
            "fetching" => InboundFileState::Fetching,
            "done" => InboundFileState::Done,
            "failed" => InboundFileState::Failed,
            other => return Err(anyhow!("unknown inbound file state: {other}")),
        })
    }
}

/// One row of `sync_outbound_files`: the durable per-file state of a file within
/// an outbound batch (§D4). Keyed uniquely on `(outbound_id, rel_path)`.
/// `bytes_done` is checkpointed on state transitions and serve ticks; `outcome`
/// carries the receiver's per-frame verdict text (same encoding as
/// [`sync_receipts`](super::store::DDL_RECEIPTS)) once the ack lands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboundFileRow {
    /// Parent `sync_outbound.id`.
    pub outbound_id: i64,
    /// Path of this file relative to the batch root (structured per §D2).
    pub rel_path: String,
    pub byte_size: u64,
    pub frame_uuid: String,
    pub state: OutboundFileState,
    /// Cumulative bytes served so far (0 until the serve stage reports progress).
    pub bytes_done: u64,
    /// The receiver's per-frame verdict text once the ack lands, else `None`.
    pub outcome: Option<String>,
    /// A per-file error detail when this file's send fails, else `None`.
    pub error: Option<String>,
    pub updated_at: String,
}

/// One row of `sync_inbound_files`: the durable per-file state of a file within
/// an inbound batch (§D4). Keyed uniquely on `(inbound_id, rel_path)`; created at
/// announce time from the v2 manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboundFileRow {
    /// Parent `sync_inbound.id`.
    pub inbound_id: i64,
    /// Path of this file relative to the batch root (from the announce manifest).
    pub rel_path: String,
    pub byte_size: u64,
    pub frame_uuid: String,
    pub state: InboundFileState,
    /// Cumulative bytes fetched so far (0 until the fetch stage reports progress).
    pub bytes_done: u64,
    /// The local ingest verdict text once known, else `None`.
    pub outcome: Option<String>,
    /// A per-file error detail when this file's fetch/ingest fails, else `None`.
    pub error: Option<String>,
    pub updated_at: String,
}

/// One row of `sync_events`: a single timestamped entry in a batch's event
/// journal (§D7 — the detail pane's **Log** tab). The `(direction, batch_key)`
/// pair scopes a journal to one batch on one side; the table keeps only the
/// newest ~200 rows per pair (see [`append_sync_event`](super::store::append_sync_event)).
/// `batch_key` is not carried on the struct — it is the query scope, not per-row
/// data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncEventRow {
    pub id: i64,
    /// Which half of the transfer this journal belongs to
    /// ([`Sent`](Direction::Sent) / [`Received`](Direction::Received)).
    pub direction: Direction,
    /// RFC3339 UTC timestamp (rendered like the other sync columns).
    pub ts: String,
    /// Short snake_case event kind (`announce_sent`, `dial_failed`,
    /// `retry_scheduled`, `serve_start`, `uploaded`, `ack_received`,
    /// `fetch_start`, `fetch_done`, `ingest_done`, `cancel`, `reject`, …).
    pub kind: String,
    /// Optional human detail (e.g. a class-tagged error). `None` for a bare event.
    pub detail: Option<String>,
}
