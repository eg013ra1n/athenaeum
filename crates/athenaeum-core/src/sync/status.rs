//! Aggregated status snapshot for the Transfers UI (task M3).
//!
//! [`SyncStatus`] is the single payload the `get_sync_status` command returns —
//! it rolls up the send-side engine's live picture and the receive-side runtime
//! into one poll. It is built by [`crate::api::sync::get_status`], which is the
//! only place that reads the sender/receiver runtimes and the catalog together.

use serde::Serialize;

use super::models::OutboundState;

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
    pub active: bool,
    /// Total frames received (history rows with `direction = received`).
    pub received_total: u32,
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
