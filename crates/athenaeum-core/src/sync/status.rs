//! Aggregated status snapshot for the Transfers UI (task M3).
//!
//! [`SyncStatus`] is the single payload the `get_sync_status` command returns —
//! it rolls up the send-side engine's live picture and the receive-side runtime
//! into one poll. It is built by [`crate::api::sync::get_status`], which is the
//! only place that reads the sender/receiver runtimes and the catalog together.

use serde::Serialize;

use super::models::{InboundState, OutboundState};

/// One in-flight outbound package for the Active tab (never a terminal row —
/// those are summarized by the counts and live in history).
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct OutboundSummary {
    /// Durable `sync_outbound` row id (stable across the package's lifecycle;
    /// the sender's `sync-progress`/`sync-finished` events key on it too).
    pub id: i64,
    /// Short, human-readable package handle (basename of the package dir).
    pub package_short: String,
    pub state: OutboundState,
    pub attempts: u32,
    pub created_at: String,
    /// Destination peer node id (hex), shortened for display.
    pub peer_short: String,
    /// The most recent failed-attempt reason (Task 9), or `None` when the package
    /// has never failed / was cleared on a successful announce or confirm.
    pub last_error: Option<String>,
    /// Wall-clock deadline (RFC3339 UTC) of the next scheduled retry (Task 2), or
    /// `None` when the package is not currently waiting out a backoff window (it is
    /// awaiting an ack or terminal). Drives the Transfers UI's live countdown.
    pub next_retry_at: Option<String>,
    /// Total payload bytes across the package's manifest (Task 14).
    pub byte_size: u64,
    /// Number of frames/files in the package's manifest (Task 14).
    pub file_count: u32,
}

/// One in-flight inbound package for the receive-side Active tab (Task 14) — the
/// mirror of [`OutboundSummary`], built from a non-terminal `sync_inbound` row.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct InboundSummary {
    /// Durable `sync_inbound` row id — the handle
    /// [`list_transfer_files`](crate::api::sync::list_transfer_files) resolves the
    /// received-detail read by.
    pub id: i64,
    /// The full wire package id (`sync_inbound.package_id`) — the exact key
    /// `cancel_incoming_package` matches on (`WHERE package_id = ?1`). A row
    /// surfaced only by the status poll (e.g. announced/fetching from a prior
    /// session) has no other way for the caller to obtain the full id, since
    /// [`package_short`](Self::package_short) is a truncated display string.
    pub package_id: String,
    /// Short, human-readable package handle (leading chars of the wire package id).
    pub package_short: String,
    /// Sending peer node id (hex), shortened for display.
    pub peer_short: String,
    pub state: InboundState,
    pub frame_count: u32,
    pub byte_size: u64,
    /// Cumulative bytes fetched so far (0 until the fetch stage reports progress).
    pub bytes_done: u64,
    pub created_at: String,
}

/// Send-side rollup: live in-flight counts from the engine's non-terminal
/// snapshot plus terminal totals counted straight from `sync_outbound`.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SyncSenderStatus {
    /// Whether the sender engine has been lazily started this session.
    pub started: bool,
    /// Non-terminal packages not yet in the transferring window (`queued` /
    /// `announced`).
    pub queued: u32,
    /// Packages in the in-flight transferring window (awaiting the peer ack).
    pub transferring: u32,
    /// Terminal `confirmed` package count (all frames ingested-or-duplicate).
    pub confirmed_total: u32,
    /// Terminal `failed` package count (local-unrecoverable payload only).
    pub failed_total: u32,
    /// Terminal `cancelled` package count (user-cancelled sends).
    pub cancelled_total: u32,
    /// The in-flight rows for the Active tab.
    pub active: Vec<OutboundSummary>,
}

/// Receive-side rollup for the down arrow + Active tab.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SyncReceiverStatus {
    /// Whether the receiver transport is running (a ticket has been minted).
    pub started: bool,
    /// The in-flight inbound rows for the receive-side Active tab (non-terminal
    /// `sync_inbound` rows, Task 14). Empty when nothing is being received.
    pub active: Vec<InboundSummary>,
    /// Total frames received (history rows with `direction = received`).
    pub received_total: u32,
}

/// Per-file detail for one transfer batch (Task 14), for the Transfers UI's
/// expand-a-row view. Built by
/// [`list_transfer_files`](crate::api::sync::list_transfer_files) from the
/// package's manifest joined to this node's per-frame history/receipt records.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct TransferFileEntry {
    /// The file's basename within the package.
    pub name: String,
    /// The file's payload size in bytes (from the manifest).
    pub bytes_total: u64,
    /// Cumulative bytes received for this file, when known (incoming detail); the
    /// live per-file bars are event-driven via `sync-file-progress`, so this is
    /// `None` mid-fetch and populated from history once the package is terminal.
    pub bytes_done: Option<u64>,
    /// Per-frame outcome once settled: outgoing — the peer's ack verdict recorded
    /// in this sender's confirmed history (`ingested`/`duplicate`/`rejected`/…),
    /// `None` while the send is still in flight; incoming — the receiver's verdict
    /// from history. `None` when not yet known.
    pub outcome: Option<String>,
}

/// The full snapshot the Transfers UI polls. Enriched in task M3 from the
/// original receive-only shape (`devPairingEnabled` / `transportStarted` /
/// `pairingTicket` / `receivedTotal` retained for back-compat) with the
/// [`sender`](Self::sender) rollup and a symmetric [`receiver`](Self::receiver)
/// rollup.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    /// Whether the dev pairing flag (`sync.dev_ticket_pairing`) is enabled.
    pub dev_pairing_enabled: bool,
    /// Whether the receiver transport is running (a ticket has been minted).
    pub transport_started: bool,
    /// This device's pairing ticket, once the receiver has started.
    pub pairing_ticket: Option<String>,
    /// Total frames received (history rows with `direction = received`).
    pub received_total: u32,
    /// Send-side rollup.
    pub sender: SyncSenderStatus,
    /// Receive-side rollup.
    pub receiver: SyncReceiverStatus,
}
