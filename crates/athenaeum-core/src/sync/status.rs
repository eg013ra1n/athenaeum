//! Aggregated status snapshot for the Transfers UI (task M3).
//!
//! [`SyncStatus`] is the single payload the `get_sync_status` command returns —
//! it rolls up the send-side engine's live picture and the receive-side runtime
//! into one poll. It is built by [`crate::api::sync::get_status`], which is the
//! only place that reads the sender/receiver runtimes and the catalog together.

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::models::{InboundState, OutboundState};

/// Per-file rollup for one transfer batch (Transfers Status Model v2 §D5),
/// aggregated from the per-file tables (`sync_outbound_files` /
/// `sync_inbound_files`) with ONE grouped query per direction per status poll.
///
/// `done` and `failed` are mutually exclusive; the remainder (`total - done -
/// failed`) is still in flight. See
/// [`crate::sync::store::outbound_file_counts`] for the exact SQL and the
/// precise definitions:
/// - **failed** — a file whose per-file `state` is `failed` OR whose `outcome`
///   starts with `rejected` (the receiver refused it).
/// - **done** — a file that reached `done`/`uploaded` AND was not rejected
///   (a user-cancelled file counts as `done`: terminal, not an error).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct TransferFileCounts {
    /// Total per-file rows for the batch (0 for a legacy pre-v2 batch with no
    /// per-file rows — the summary's `file_count` is the manifest fallback).
    pub total: u32,
    /// Files that finished transferring and were not rejected.
    pub done: u32,
    /// Files that failed or were rejected by the peer.
    pub failed: u32,
}

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
    /// User-facing "attempt N" generation counter (Transfers Batch Model §D5),
    /// bumped ONLY by a resend — NOT by the engine's internal announce-retries
    /// (which bump [`attempts`](Self::attempts)). The UI shows "attempt
    /// {generation}".
    pub generation: u32,
    /// The durable per-transfer batch identity (Transfers Batch Model §D1) — the
    /// package-dir basename, which B3 aligned to equal the wire `batch_uuid`. The
    /// stable key across resend attempts, and (== `sync_history.package_id` for
    /// sent rows) the `package_key` `delete_transfer_history` takes.
    pub batch_uuid: String,
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
    /// The human batch name (`sync_outbound.display_name`, §D1), or `None` for a
    /// legacy/never-named row. The Transfers UI shows this instead of the raw
    /// package handle.
    pub display_name: Option<String>,
    /// The destination peer's friendly device name, resolved from the cached
    /// `SYNC_DEVICE_NAMES` hex→name map (no hub round-trip). `None` when the peer
    /// is not in the cache.
    pub device_name: Option<String>,
    /// Backend-derived presentation state (§D5): `queued` | `preparing` |
    /// `transferring` | `uploaded` | `waiting` | `confirmed` | `cancelled` |
    /// `failed`. `waiting` (a live backoff window) WINS over the raw
    /// [`state`](Self::state) when a retry is armed for the future; the raw
    /// `state` field stays for compatibility. See
    /// [`outbound_display_state`].
    pub display_state: String,
    /// RFC3339 deadline of the armed retry when [`display_state`](Self::display_state)
    /// is `waiting`, else `None` — the countdown target.
    pub stalled_until: Option<String>,
    /// Per-file rollup for the progress line ("N of M files").
    pub file_counts: TransferFileCounts,
    /// Whether a retry is armed (`next_retry_at` is set). The frontend reads this
    /// instead of deriving "retrying" from `attempts`. Distinct from the
    /// `waiting` display-state, which additionally requires the deadline to be in
    /// the future.
    pub retrying: bool,
    /// Whether **Resend** (`retry_sync_package`) would currently succeed for this
    /// row: terminal `failed`/`cancelled` AND the package dir still has its
    /// manifest + payload on disk (the same guard `retry_sync_package` itself
    /// enforces). Always `false` on a live (non-terminal) row from the 10s
    /// `get_sync_status` poll — that path deliberately never fs-stats the package
    /// dir to compute this (a live row never shows Resend anyway) — and defaults
    /// `false` in the shared mapper; only [`crate::api::sync::list_terminal_transfers`]
    /// sets it per row, since retention can delete a confirmed/old package's
    /// payload after the fact, leaving a terminal row whose Resend button would
    /// otherwise dead-end in "data missing on disk".
    pub resendable: bool,
}

/// The outbound presentation state (§D5), derived from the raw
/// [`OutboundState`] and the armed-retry deadline. `waiting` (a live backoff
/// window — neutral, not an error) WINS over the raw state when the package is
/// non-terminal AND `next_retry_at` is set AND that deadline is still in the
/// future; an armed-but-past deadline falls through to the raw state.
pub fn outbound_display_state(
    state: OutboundState,
    next_retry_at: Option<&str>,
    now: DateTime<Utc>,
) -> String {
    if !state.is_terminal() {
        if let Some(ts) = next_retry_at {
            if let Ok(deadline) = DateTime::parse_from_rfc3339(ts) {
                if deadline.with_timezone(&Utc) > now {
                    return "waiting".to_string();
                }
            }
        }
    }
    match state {
        OutboundState::Queued => "queued",
        OutboundState::Announced => "preparing",
        OutboundState::Transferring => "transferring",
        OutboundState::Delivered => "uploaded",
        OutboundState::Confirmed => "confirmed",
        OutboundState::Failed => "failed",
        OutboundState::Cancelled => "cancelled",
    }
    .to_string()
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
    /// User-facing "attempt N" generation counter (Transfers Batch Model §D5),
    /// bumped by each receiver re-attempt of the same `(peer, batch_uuid)` — NOT
    /// by anything else. The UI shows "attempt {generation}".
    pub generation: u32,
    /// The durable per-transfer batch identity (Transfers Batch Model §D1) the
    /// receiver keys its ONE long-lived row on (`sync_inbound.batch_uuid`).
    /// Falls back to the wire package id for a legacy row whose `batch_uuid`
    /// column is NULL (a v1/v2 receive, or a row pre-dating the column).
    pub batch_uuid: String,
    pub frame_count: u32,
    pub byte_size: u64,
    /// Cumulative bytes fetched so far (0 until the fetch stage reports progress).
    pub bytes_done: u64,
    pub created_at: String,
    /// The human batch name (`sync_inbound.display_name`, §D1) carried in the v2
    /// announce, or `None` for a v1/unnamed batch. Shown instead of the raw
    /// package id.
    pub display_name: Option<String>,
    /// The sending peer's friendly device name, resolved from the cached
    /// `SYNC_DEVICE_NAMES` hex→name map (no hub round-trip). `None` when unknown.
    pub device_name: Option<String>,
    /// Backend-derived presentation state (§D5): `announced` | `fetching` |
    /// `ingesting` | `done` | `cancelled` | `failed`. Currently mirrors the raw
    /// [`state`](Self::state) (the receiver has no `waiting`/backoff concept yet).
    pub display_state: String,
    /// Always `None` for inbound in v2 (no receiver-side retry backoff yet);
    /// present for shape-parity with [`OutboundSummary`].
    pub stalled_until: Option<String>,
    /// Per-file rollup for the progress line ("N of M files").
    pub file_counts: TransferFileCounts,
    /// The terminal failure/cancel reason (`sync_inbound.last_error`, B5b) — e.g.
    /// `"by sender"` on a sender-revoked cancel, `"sender failed"` on a sender
    /// failure, `"nothing to fetch (superseded by sender)"` on a supersede, or an
    /// ingest/fetch/ack failure reason. `None` on a healthy in-flight row. Lets a
    /// terminal received row (surfaced via
    /// [`list_terminal_transfers`](crate::api::sync::list_terminal_transfers)) say
    /// WHY it ended instead of a bare `cancelled`/`failed`.
    pub last_error: Option<String>,
    /// The sending peer's device capability — `"athenaeum"` (a full peer) or
    /// `"perseus"` (a send-only capture agent) — for the Transfers UI's
    /// per-transfer origin badge (Perseus UI v2). Resolved by the
    /// [`inbound_summary`](crate::api::sync) mapper: the value stamped onto the
    /// `sync_inbound` row at announce time (`peer_capability`, Task 9) wins; a
    /// legacy row whose stamp is NULL falls back to the cached
    /// `SYNC_PEER_CAPABILITIES` hex→kind map keyed on the sending peer. `None`
    /// when neither source knows the peer. Informational only — never gates a
    /// transfer.
    pub peer_kind: Option<String>,
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
    /// The file's path relative to the batch root (forward-slash, structured per
    /// §D2) — the primary key of the per-file tables (Transfers Status Model v2).
    /// Drives the detail pane's collapsible directory tree. For a legacy pre-v2
    /// batch with no per-file rows this falls back to the manifest `rel_path`
    /// (sent) or the history `filename` (received).
    pub rel_path: String,
    /// The file's basename within the package (kept for compat).
    pub name: String,
    /// The file's payload size in bytes (from the per-file row / manifest).
    pub bytes_total: u64,
    /// Cumulative bytes transferred for this file, when known; `None` on the
    /// legacy fallback path where no per-file row exists.
    pub bytes_done: Option<u64>,
    /// Per-file lifecycle state (`pending`/`sending`/`uploaded`/`done` outgoing;
    /// `announced`/`fetching`/`done`/`failed` incoming), from the per-file row.
    /// `None` on the legacy fallback path.
    pub state: Option<String>,
    /// Per-frame outcome once settled: outgoing — the peer's ack verdict
    /// (`ingested`/`duplicate`/`rejected`/…); incoming — this node's ingest
    /// verdict. `None` while still in flight / not yet known.
    pub outcome: Option<String>,
    /// A per-file error detail when this file's transfer failed, else `None`.
    pub error: Option<String>,
}

/// Queryable transport-health surface for the Transfers UI (Task 3.3): whether
/// this node can currently be reached by remote peers. Derived on each status
/// poll with NO network I/O — from the bound iroh node's relay watcher +
/// one-shot `online()`-wait outcome (a bound node reports `relay_connected` /
/// `direct_only`), or, with no node bound, from local sign-in + cached-relay
/// state (`not_started` / `no_relay_map`). Built by
/// [`crate::api::sync::derive_transport_health`].
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct TransportHealth {
    /// One of `"not_started"` (no transport bound yet), `"relay_connected"`
    /// (a home relay is up — remote peers can be reached), `"direct_only"` (the
    /// relay is disabled or unreachable — peers behind NAT may not reach us), or
    /// `"no_relay_map"` (signed in but no relay configuration resolved or cached —
    /// remote transfers will stall).
    pub status: String,
    /// The home-relay URL, when one is known (currently connected, or the last
    /// one the watcher saw). `None` before any relay transition / with no relay.
    pub relay_url: Option<String>,
    /// The most recent relay error, when the last transition was a disconnect.
    /// `None` on a healthy relay or when the disconnect carried no error.
    pub last_error: Option<String>,
}

impl TransportHealth {
    /// No transport has been bound yet this session.
    pub fn not_started() -> Self {
        Self { status: "not_started".into(), relay_url: None, last_error: None }
    }

    /// Signed in, but no relay configuration was resolved or cached — a bound
    /// transport could not reach remote peers.
    pub fn no_relay_map() -> Self {
        Self { status: "no_relay_map".into(), relay_url: None, last_error: None }
    }

    /// A home relay is connected; remote peers can reach this node.
    pub fn relay_connected(relay_url: Option<String>) -> Self {
        Self { status: "relay_connected".into(), relay_url, last_error: None }
    }

    /// Direct addresses only — the relay is disabled or its wait timed out, so
    /// peers behind NAT may be unreachable.
    pub fn direct_only(relay_url: Option<String>, last_error: Option<String>) -> Self {
        Self { status: "direct_only".into(), relay_url, last_error }
    }
}

/// The full snapshot the Transfers UI polls. Enriched in task M3 from the
/// original receive-only shape (`devPairingEnabled` / `transportStarted` /
/// `pairingTicket` / `receivedTotal` retained for back-compat) with the
/// [`sender`](Self::sender) rollup and a symmetric [`receiver`](Self::receiver)
/// rollup, and in Task 3.3 with the [`transport`](Self::transport) health line.
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
    /// Transport reachability health (Task 3.3): relay connected / direct-only /
    /// no relay map / not started. Drives the sidebar badge's health dot.
    pub transport: TransportHealth,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    /// RFC3339 stamp `secs` from `base` (positive = future, negative = past),
    /// formatted exactly like the engine's `next_retry_at`.
    fn offset(base: DateTime<Utc>, secs: i64) -> String {
        (base + Duration::seconds(secs)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    #[test]
    fn outbound_display_state_maps_every_raw_state_without_retry() {
        let now = Utc::now();
        let cases = [
            (OutboundState::Queued, "queued"),
            (OutboundState::Announced, "preparing"),
            (OutboundState::Transferring, "transferring"),
            (OutboundState::Delivered, "uploaded"),
            (OutboundState::Confirmed, "confirmed"),
            (OutboundState::Failed, "failed"),
            (OutboundState::Cancelled, "cancelled"),
        ];
        for (state, expected) in cases {
            assert_eq!(outbound_display_state(state, None, now), expected, "{state:?}");
        }
    }

    #[test]
    fn armed_future_retry_wins_over_raw_state_for_non_terminal() {
        let now = Utc::now();
        let future = offset(now, 30);
        // Every non-terminal raw state → waiting when a future retry is armed.
        for state in [
            OutboundState::Queued,
            OutboundState::Announced,
            OutboundState::Transferring,
            OutboundState::Delivered,
        ] {
            assert_eq!(
                outbound_display_state(state, Some(&future), now),
                "waiting",
                "{state:?} with a future retry should be waiting"
            );
        }
    }

    #[test]
    fn armed_past_retry_does_not_win() {
        let now = Utc::now();
        let past = offset(now, -30);
        assert_eq!(outbound_display_state(OutboundState::Transferring, Some(&past), now), "transferring");
        assert_eq!(outbound_display_state(OutboundState::Announced, Some(&past), now), "preparing");
    }

    #[test]
    fn terminal_states_never_show_waiting_even_if_armed() {
        let now = Utc::now();
        let future = offset(now, 30);
        // A terminal row never shows waiting, even if a stale next_retry_at lingers.
        assert_eq!(outbound_display_state(OutboundState::Confirmed, Some(&future), now), "confirmed");
        assert_eq!(outbound_display_state(OutboundState::Failed, Some(&future), now), "failed");
        assert_eq!(outbound_display_state(OutboundState::Cancelled, Some(&future), now), "cancelled");
    }

    #[test]
    fn unparseable_retry_deadline_falls_through_to_raw_state() {
        let now = Utc::now();
        assert_eq!(
            outbound_display_state(OutboundState::Transferring, Some("not-a-date"), now),
            "transferring"
        );
    }
}
