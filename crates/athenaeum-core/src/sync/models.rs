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
/// ack. Terminal states are [`Confirmed`](Self::Confirmed) and
/// [`Failed`](Self::Failed); everything else is non-terminal and re-driven on
/// crash-resume.
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
    /// until the peer accepts every frame, or `max_attempts` exhausts to
    /// `Failed`.
    Confirmed,
    /// Retries exhausted or cancelled — terminal failure. Also reached when a
    /// package keeps receiving a partial/rejected ack past `max_attempts`; the
    /// recorded history names the frame(s) that were still rejected.
    Failed,
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
            other => return Err(anyhow!("unknown outbound state: {other}")),
        })
    }

    /// True for the two terminal states ([`Confirmed`](Self::Confirmed),
    /// [`Failed`](Self::Failed)) that crash-resume must *not* re-drive.
    pub fn is_terminal(&self) -> bool {
        matches!(self, OutboundState::Confirmed | OutboundState::Failed)
    }
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
    pub limit: u32,
}
