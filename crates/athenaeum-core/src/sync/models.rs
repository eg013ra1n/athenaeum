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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Peer acked receipt — terminal success.
    Confirmed,
    /// Retries exhausted or cancelled — terminal failure.
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
    /// Short outcome tag: `sent`, `ingested`, `duplicate`, `rejected`, `failed`,
    /// or `cancelled`.
    pub outcome: String,
}

/// Minimal query surface for [`SyncStore::search_history`](super::store::SyncStore::search_history).
///
/// YAGNI by design (task A4): exact-match filters on `filename` and/or `object`
/// (both optional; `None` = unfiltered), newest-first, capped at `limit`.
#[derive(Debug, Clone, Default)]
pub struct HistoryQuery {
    pub filename: Option<String>,
    pub object: Option<String>,
    pub limit: u32,
}
