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
/// - **duplicate** — a file the peer already held (`outcome = 'duplicate'`), a
///   SUBSET of `done` (and of `total`), never a sibling. On the send side these
///   are the §D4 want-subset exclusions: settled `done` at negotiate, never
///   transferred — so a raw "done of total" over the full manifest reads
///   346 of 562 while the receiver, whose announce carries only the want
///   subset, reads 84 of 300. The sender's progress line subtracts them (files
///   AND bytes) and says where the rest went ("84 of 300 · 262 already on
///   peer"). A receiver-ingest duplicate (travelled, then found in the catalog)
///   lands here too once the ack settles it; inbound rows carry only that kind,
///   and the receive-side UI ignores the field.
/// - **total_bytes** — the `byte_size` sum of EVERY row, whatever its state.
///   Not a progress figure: it is the batch's declared size, known from the
///   per-file rows alone, which is what a `preparing` row's summary falls back
///   to while its payload dir (and therefore its manifest) does not exist yet
///   (transfer-prepare spec §3.8).
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
    /// Files the peer already held (`outcome = 'duplicate'`) — counted INSIDE
    /// `total` and `done`; see the struct doc for the send-side split.
    pub duplicate: u32,
    /// `byte_size` sum of the `duplicate` rows — subtract from the summary's
    /// manifest-total `byte_size` to get the bytes that actually travel.
    pub duplicate_bytes: u64,
    /// `byte_size` sum of every row — the manifest-free total a `preparing`
    /// row's summary falls back to (spec §3.8).
    pub total_bytes: u64,
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
    // NOTE (plain comment on purpose — a `///` line here regenerates
    // `src/types/models.ts` through ts-rs, and the TS/presentation half is a
    // separate dispatch): the vocabulary above is NOT exhaustive. It predates
    // `waiting_peer` (D1) and `queued_at_receiver` (variant A), both of which this
    // field can carry. [`outbound_display_state`] is the canonical list and the
    // full precedence; keep reading there, not here.
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

/// The machine-readable class prefixes a failed dial writes into `last_error`
/// (`sync::diagnostics::ConnectClass::tag`) that mean "the peer is not there"
/// (D1 §3.3). Kept in step with the engine's own set by
/// `absent_prefixes_match_the_engine_classes` — they are the same fact spelled
/// twice, on opposite sides of the store.
const PEER_ABSENT_PREFIXES: [&str; 4] = [
    "no_route:",
    "timeout:",
    "relay_unreachable:",
    "not_started:",
];

/// Whether this row's recorded failure says the PEER is absent, as opposed to
/// refusing us or our own side being broken.
pub fn peer_looks_absent(last_error: Option<&str>) -> bool {
    last_error.is_some_and(|e| PEER_ABSENT_PREFIXES.iter().any(|p| e.starts_with(p)))
}

/// The outbound presentation state (§D5), derived from the raw
/// [`OutboundState`], the armed-retry deadline, the recorded failure class, and
/// whether the destination peer is busy pulling a SIBLING batch of ours right now.
///
/// Precedence, non-terminal rows only:
///
/// 1. **`waiting_peer`** (D1) wins first: a package parked because its peer is
///    absent is waiting for a SIGNAL, not for an instant, so rendering a countdown
///    would put a number on the screen that means nothing — the transfer resumes
///    the moment the peer announces itself, which may be seconds or hours from now.
///    It also beats `receiver_busy`, which is the weaker (and possibly stale) fact:
///    "queued at the receiver" claims the receiver is alive and working, and a
///    peer-absent dial says it is not.
/// 2. **`queued_at_receiver`** (variant A) when `receiver_busy`: the receiver runs
///    ONE transfer per peer at a time, so a second batch's announce succeeds (it
///    lands in the peer's lane queue) and then sits there — durable state cannot
///    tell that apart from trouble, since both batches read `Transferring`. Without
///    this the row hits the 30s ack timeout, arms `next_retry_at`, and renders
///    "waiting · retry in N" — a countdown that reads like connectivity failure
///    while the receiver is in fact actively pulling our other batch.
/// 3. **`waiting`** (a live backoff window — neutral, not an error) then wins over
///    the raw state when `next_retry_at` is set AND that deadline is still in the
///    future; an armed-but-past deadline falls through to the raw state.
///
/// `receiver_busy` means: a sibling package addressed to this row's peer is being
/// served BY US right now, and this row is not that package (the api layer enforces
/// the "not that package" half — see `crate::api::sync::row_receiver_busy`). It is
/// a live, decaying signal: once the peer stops pulling it goes false and the
/// ack-timeout ladder takes over exactly as it does today.
///
/// Which raw states it may relabel, and why:
/// - `Transferring` — **yes**, the classic case: announced, in the peer's lane,
///   waiting its turn behind a sibling.
/// - `Announced` — **yes**: the announce is out and the peer has it; the reason no
///   pull has started is the same serial lane.
/// - `Queued` — **no**: nothing has been announced yet, so the receiver has never
///   heard of this batch and cannot be queueing it. The queue is ours, and plain
///   `queued` already says so.
/// - `Delivered` — **no**: the bytes are fully uploaded and we are awaiting the
///   confirmation. `uploaded` is strictly more informative than "queued", and the
///   receiver being busy elsewhere doesn't change what has already left.
/// - terminal states — **no**, like every other non-raw label here.
pub fn outbound_display_state(
    state: OutboundState,
    next_retry_at: Option<&str>,
    last_error: Option<&str>,
    receiver_busy: bool,
    now: DateTime<Utc>,
) -> String {
    if !state.is_terminal() {
        if peer_looks_absent(last_error) {
            return "waiting_peer".to_string();
        }
        if receiver_busy
            && matches!(
                state,
                OutboundState::Announced | OutboundState::Transferring
            )
        {
            return "queued_at_receiver".to_string();
        }
        if let Some(ts) = next_retry_at {
            if let Ok(deadline) = DateTime::parse_from_rfc3339(ts) {
                if deadline.with_timezone(&Utc) > now {
                    return "waiting".to_string();
                }
            }
        }
    }
    match state {
        // Transfer-prepare spec §3: `preparing` names the REAL pre-announce
        // stage now (the worker is copying + hashing the payload), so an
        // announced package — whose bytes are staged and offered — says
        // `announced` instead of borrowing that label.
        OutboundState::Preparing => "preparing",
        OutboundState::Queued => "queued",
        OutboundState::Announced => "announced",
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
    /// Backend-derived presentation state (§D5). Mirrors the raw
    /// [`state`](Self::state) except: `Waiting` renders `waiting_peer` (D2 — one
    /// chip for both directions) and an `Announced` row parked for a receive slot
    /// renders `queued` (variant C). The vocabulary is NOT exhaustive here — the
    /// mapper in `api::sync::inbound_summary` is canonical.
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

/// One announce QUEUED on a peer's receive lane but not yet processed (variant
/// B) — a batch that exists only in memory, since the receiver writes no
/// `sync_inbound` row until its lane picks the announce up.
///
/// The receive side runs one transfer per peer at a time, so a device that sends
/// a second batch while the first is still fetching would otherwise show NOTHING
/// on the receiver until the first finishes. This is the ghost row that says
/// "queued behind the current transfer from this device" instead.
///
/// Deliberately NOT an [`InboundSummary`]: it has no durable row id, no state, no
/// per-file counts and no progress — there is nothing to cancel, expand or resume
/// yet. Giving it the real summary's shape would invite the UI to treat it as a
/// real row. Everything here comes off the wire announce.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct QueuedInboundSummary {
    /// Sending peer node id (hex), shortened for display — same `short_id` handle
    /// [`InboundSummary::peer_short`] carries.
    pub peer_short: String,
    /// The sending peer's friendly device name, resolved from the cached
    /// `SYNC_DEVICE_NAMES` hex→name map (no hub round-trip). `None` when unknown.
    pub device_name: Option<String>,
    /// The durable per-transfer batch identity (Transfers Batch Model §D1) the
    /// inbound row WILL be keyed on once this announce is processed — so the ghost
    /// and the row it becomes share one key.
    pub batch_uuid: String,
    /// The human batch name from the announce, `None` for a v1/unnamed batch.
    pub batch_name: Option<String>,
    pub frame_count: u32,
    pub byte_size: u64,
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
    /// Announces routed to a peer's lane but not yet processed (variant B): the
    /// batches queued behind whatever that device is currently sending us. Empty
    /// when the receiver is not started. Never contains a batch that already
    /// appears in [`active`](Self::active) — the `queued_inbound_summaries` mapper
    /// in `api::sync` drops those (a sender re-announces the in-flight batch on
    /// every backoff rung, and a ghost must never sit next to its own live row).
    pub queued: Vec<QueuedInboundSummary>,
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
        Self {
            status: "not_started".into(),
            relay_url: None,
            last_error: None,
        }
    }

    /// Signed in, but no relay configuration was resolved or cached — a bound
    /// transport could not reach remote peers.
    pub fn no_relay_map() -> Self {
        Self {
            status: "no_relay_map".into(),
            relay_url: None,
            last_error: None,
        }
    }

    /// A home relay is connected; remote peers can reach this node.
    pub fn relay_connected(relay_url: Option<String>) -> Self {
        Self {
            status: "relay_connected".into(),
            relay_url,
            last_error: None,
        }
    }

    /// Direct addresses only — the relay is disabled or its wait timed out, so
    /// peers behind NAT may be unreachable.
    pub fn direct_only(relay_url: Option<String>, last_error: Option<String>) -> Self {
        Self {
            status: "direct_only".into(),
            relay_url,
            last_error,
        }
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

    /// D1: a package parked because its peer is absent reads as `waiting_peer` and
    /// carries NO countdown — it resumes on a signal, and a number would be a lie.
    #[test]
    fn a_peer_absent_row_reads_as_waiting_for_the_peer() {
        let now = Utc::now();
        let future = (now + chrono::Duration::seconds(120)).to_rfc3339();
        for err in [
            "no_route: no known addresses",
            "timeout: dial timed out",
            "relay_unreachable: relay leg failed",
            "not_started: peer not started",
        ] {
            assert_eq!(
                outbound_display_state(OutboundState::Queued, None, Some(err), false, now),
                "waiting_peer",
                "{err}"
            );
            // Even with a countdown persisted (the probing head has one), the peer
            // fact wins: the row says who it is waiting for, not when it will try.
            assert_eq!(
                outbound_display_state(
                    OutboundState::Announced,
                    Some(&future),
                    Some(err),
                    false,
                    now
                ),
                "waiting_peer",
                "{err}"
            );
        }
    }

    /// Variant A, the whole point: a second batch to a receiver that is busy pulling
    /// a SIBLING says so, instead of counting down a retry the user reads as trouble.
    /// The countdown is armed (the ack timed out — the receiver's lane is serial and
    /// this batch is behind another), and `queued_at_receiver` beats it.
    #[test]
    fn queued_at_receiver_wins_over_the_countdown_when_a_sibling_is_served() {
        let now = Utc::now();
        let future = offset(now, 90);
        for state in [OutboundState::Announced, OutboundState::Transferring] {
            assert_eq!(
                outbound_display_state(state, Some(&future), None, true, now),
                "queued_at_receiver",
                "{state:?} behind a served sibling must not read as a countdown"
            );
            // With no retry armed at all it is still the honest label.
            assert_eq!(
                outbound_display_state(state, None, None, true, now),
                "queued_at_receiver",
                "{state:?}"
            );
        }
    }

    /// Honest-labeling guard: the package the receiver is ACTUALLY pulling is never
    /// the one queued behind something. The api layer encodes that by passing
    /// `receiver_busy = false` for the active row (see
    /// `crate::api::sync::row_receiver_busy`); here we pin the pure half — the same
    /// row reads as its raw/waiting self the moment the flag is off.
    #[test]
    fn the_actively_served_package_never_reads_queued() {
        let now = Utc::now();
        let future = offset(now, 90);
        assert_eq!(
            outbound_display_state(OutboundState::Transferring, None, None, false, now),
            "transferring",
            "the moving package reads as moving"
        );
        assert_eq!(
            outbound_display_state(OutboundState::Transferring, Some(&future), None, false, now),
            "waiting",
            "and with a retry armed it keeps today's countdown exactly"
        );
    }

    /// A stale busy signal must never overwrite the stronger fact that the peer is
    /// gone: "queued at the receiver" claims the receiver is alive and working, and
    /// a peer-absent dial says it is not.
    #[test]
    fn peer_absent_beats_receiver_busy() {
        let now = Utc::now();
        assert_eq!(
            outbound_display_state(
                OutboundState::Transferring,
                None,
                Some("no_route: no known addresses"),
                true,
                now
            ),
            "waiting_peer"
        );
    }

    /// The label describes a queue at the RECEIVER, so it only applies to a batch we
    /// have actually announced and not yet finished uploading. `Queued` is our own
    /// local queue (nothing announced — the receiver has never heard of it) and
    /// `Delivered` means the bytes are fully uploaded ("uploaded — awaiting
    /// confirmation" is strictly more informative than "queued"). Neither is
    /// relabeled, busy sibling or not.
    #[test]
    fn local_queue_and_uploaded_states_are_not_relabeled() {
        let now = Utc::now();
        assert_eq!(
            outbound_display_state(OutboundState::Queued, None, None, true, now),
            "queued",
            "not announced yet — the receiver has nothing of ours to queue"
        );
        assert_eq!(
            outbound_display_state(OutboundState::Delivered, None, None, true, now),
            "uploaded",
            "bytes are up; 'uploaded' says more than 'queued'"
        );
        // Terminal rows are untouched by the flag as well.
        for state in [
            OutboundState::Confirmed,
            OutboundState::Failed,
            OutboundState::Cancelled,
        ] {
            let s = outbound_display_state(state, None, None, true, now);
            assert_ne!(s, "queued_at_receiver", "{state:?} is terminal");
        }
    }

    /// A peer that is UP and refusing us, or a failure we could not classify, is a
    /// different fact — those keep the ordinary schedule and its countdown.
    #[test]
    fn a_refusing_or_unclassified_failure_is_not_peer_absent() {
        let now = Utc::now();
        let future = (now + chrono::Duration::seconds(60)).to_rfc3339();
        assert_eq!(
            outbound_display_state(
                OutboundState::Queued,
                Some(&future),
                Some("refused: not on the peer's allow-list"),
                false,
                now
            ),
            "waiting"
        );
        assert_eq!(
            outbound_display_state(
                OutboundState::Queued,
                Some(&future),
                Some("other: disk full"),
                false,
                now
            ),
            "waiting"
        );
    }

    /// A TERMINAL row never reads as waiting for anyone, whatever stale reason it
    /// still carries.
    #[test]
    fn a_terminal_row_ignores_a_stale_absent_reason() {
        let now = Utc::now();
        for state in [
            OutboundState::Failed,
            OutboundState::Cancelled,
            OutboundState::Confirmed,
        ] {
            let s = outbound_display_state(state, None, Some("no_route: gone"), false, now);
            assert_ne!(s, "waiting_peer", "{state:?} is terminal");
        }
    }

    /// Drift guard: the prefixes this mapper treats as "peer absent" must be
    /// exactly the classes the ENGINE schedules flat. They are the same fact on
    /// opposite sides of the store, and nothing but this test ties them together.
    #[test]
    fn absent_prefixes_match_the_engine_classes() {
        use crate::sync::diagnostics::ConnectClass;
        for class in [
            ConnectClass::NoRoute,
            ConnectClass::Timeout,
            ConnectClass::RelayUnreachable,
            ConnectClass::NotStarted,
        ] {
            let rendered = format!("{}: something", class.tag());
            assert!(
                peer_looks_absent(Some(&rendered)),
                "{} is flat-scheduled by the engine but not treated as absent here",
                class.tag()
            );
        }
        for class in [ConnectClass::Refused, ConnectClass::Other] {
            let rendered = format!("{}: something", class.tag());
            assert!(
                !peer_looks_absent(Some(&rendered)),
                "{} escalates in the engine and must not read as absent",
                class.tag()
            );
        }
    }

    #[test]
    fn outbound_display_state_maps_every_raw_state_without_retry() {
        let now = Utc::now();
        let cases = [
            (OutboundState::Queued, "queued"),
            // Transfer-prepare spec §3: `preparing` is now the REAL staging
            // stage; an announced row has left the building and says so.
            (OutboundState::Preparing, "preparing"),
            (OutboundState::Announced, "announced"),
            (OutboundState::Transferring, "transferring"),
            (OutboundState::Delivered, "uploaded"),
            (OutboundState::Confirmed, "confirmed"),
            (OutboundState::Failed, "failed"),
            (OutboundState::Cancelled, "cancelled"),
        ];
        for (state, expected) in cases {
            assert_eq!(
                outbound_display_state(state, None, None, false, now),
                expected,
                "{state:?}"
            );
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
                outbound_display_state(state, Some(&future), None, false, now),
                "waiting",
                "{state:?} with a future retry should be waiting"
            );
        }
    }

    #[test]
    fn armed_past_retry_does_not_win() {
        let now = Utc::now();
        let past = offset(now, -30);
        assert_eq!(
            outbound_display_state(OutboundState::Transferring, Some(&past), None, false, now),
            "transferring"
        );
        assert_eq!(
            outbound_display_state(OutboundState::Announced, Some(&past), None, false, now),
            "announced"
        );
    }

    #[test]
    fn terminal_states_never_show_waiting_even_if_armed() {
        let now = Utc::now();
        let future = offset(now, 30);
        // A terminal row never shows waiting, even if a stale next_retry_at lingers.
        assert_eq!(
            outbound_display_state(OutboundState::Confirmed, Some(&future), None, false, now),
            "confirmed"
        );
        assert_eq!(
            outbound_display_state(OutboundState::Failed, Some(&future), None, false, now),
            "failed"
        );
        assert_eq!(
            outbound_display_state(OutboundState::Cancelled, Some(&future), None, false, now),
            "cancelled"
        );
    }

    #[test]
    fn unparseable_retry_deadline_falls_through_to_raw_state() {
        let now = Utc::now();
        assert_eq!(
            outbound_display_state(
                OutboundState::Transferring,
                Some("not-a-date"),
                None,
                false,
                now
            ),
            "transferring"
        );
    }
}
