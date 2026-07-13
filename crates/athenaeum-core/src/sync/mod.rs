//! Sender-side personal-sync engine (Stage I, task A4).
//!
//! This module owns the *outbound* half of personal sync: a durable state
//! machine that drives one shareable [`package`](crate::package) from "queued to
//! send" to "confirmed received by the peer", surviving process restarts. It
//! sits directly on top of two lower layers and adds catalog/durability logic:
//!
//! - [`crate::sharing::SharingTransport`] (task A2) moves announcements, blobs,
//!   and acks between peers. The engine is transport-agnostic — it is tested end
//!   to end against the in-process
//!   [`LoopbackTransport`](crate::sharing::loopback::LoopbackTransport) and runs
//!   unchanged over the real iroh transport (task A5).
//! - [`crate::package`] (task A3) is the on-disk bundle format the engine serves
//!   and whose manifest it reads to build audit history.
//!
//! # Layout
//!
//! - [`models`] — the persisted row types ([`OutboundRow`], [`HistoryRow`]) and
//!   the [`OutboundState`] lifecycle enum.
//! - [`store`] — the [`SyncStore`](store::SyncStore) trait (one DDL, defined
//!   once as consts), the [`StandaloneSyncStore`](store::StandaloneSyncStore)
//!   rusqlite implementation (own WAL SQLite file, used by Perseus), and the
//!   catalog-backed [`CatalogSyncStore`](store::CatalogSyncStore) (task A7) over
//!   the app catalog DB — the sync DDL now lives in `db/schema.rs::init_db` too,
//!   sharing these consts.
//! - [`engine`] — [`SyncEngine`](engine::SyncEngine) / its
//!   [`SyncEngineHandle`](engine::SyncEngineHandle) and the tokio worker task
//!   that implements the sender-side state machine.
//! - [`ingest`] / [`receiver`] — the primary-side receive pipeline (task A7):
//!   [`ingest_package`](ingest::ingest_package) turns a fetched package into
//!   catalog rows + receipts, and [`SyncReceiver`](receiver::SyncReceiver) /
//!   [`SyncRuntime`](receiver::SyncRuntime) drive fetch → ingest → ack over a
//!   transport.
//!
//! # State machine (v1, collapsed)
//!
//! The full lifecycle enum is `Queued | Announced | Transferring | Delivered |
//! Confirmed | Failed`, but v1 collapses it: the sender learns a transfer
//! completed only via the peer's ack (loopback `fetch` is receiver-driven —
//! the sender sees no distinct "delivered" signal), so [`OutboundState::Delivered`]
//! is retained in the enum (and therefore in the DDL text) but never written.
//! The live path is `Queued → Announced → Transferring → Confirmed`, with
//! `Failed` terminal (max-attempts exhaustion or explicit cancel).

use anyhow::{anyhow, Result};
use chrono::Utc;

use crate::sharing::types::NodeId;

pub mod cleanup_coord;
pub mod dedup;
pub mod engine;
pub mod ingest;
pub mod models;
pub mod pairing;
pub mod project_ingest;
pub mod receiver;
pub mod responder;
pub mod retention;
pub mod sender;
pub mod status;
pub mod store;

#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod ingest_tests;
#[cfg(test)]
mod retention_tests;

pub use cleanup_coord::SharedPackageCleanup;
pub use dedup::{confirm_candidates, partition_offer, WantSplit};
pub use engine::{
    PackageCleanupSink, SyncConfig, SyncEngine, SyncEngineHandle, DEFAULT_ACK_TIMEOUT, MAX_ATTEMPTS,
};
pub use ingest::{ingest_package, IngestOutcome};
pub use pairing::{node_id_from_ticket, relay_mode_for, resolve_relays, RelayResolution};
pub use models::{Direction, HistoryQuery, HistoryRow, OutboundRow, OutboundState};
pub use project_ingest::{ingest_project_package, ProjectIngestOutcome};
pub use retention::{evaluate_and_apply, DeleteOutcome, RetentionOutcome, RetentionPolicy};
pub use receiver::{
    allow_all_peers, IncomingResolver, PeerAuthorizer, ProjectAnnounceGate,
    ProjectAnnouncementsRefresher, ProjectIngestedHook, ProjectReceiveHooks, ProjectRequestHandler,
    ReceiverHooks, SyncFinishedEvent, SyncProgressEvent, SyncReceiver, SyncReceiverHandle,
    SyncRuntime,
};
pub use responder::{CatalogDedupResponder, DedupResponder};
pub use sender::{StartedSender, SyncSenderRuntime};
pub use status::{OutboundSummary, SyncReceiverStatus, SyncSenderStatus, SyncStatus};
pub use store::{
    insert_sync_source, live_sources_for_package, mark_sync_source_deleted, CatalogSyncStore,
    StandaloneSyncStore, SyncSourceRow, SyncStore,
};

/// Canonical timestamp rendering for the sync tables: RFC3339 UTC, millisecond
/// precision, `Z` suffix. Sortable as text, unambiguous across time zones.
pub(crate) fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Lowercase-hex rendering of a 32-byte node id (64 chars), used as the stored
/// `peer` / `peer_device` column value. `pub` (not `pub(crate)`) so Perseus (an
/// external crate) shares this one implementation instead of a local copy
/// (task M1 review minor (c)).
pub fn node_id_hex(id: &NodeId) -> String {
    let mut s = String::with_capacity(64);
    for b in id {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Parse the 64-char lowercase-hex form produced by [`node_id_hex`] back into a
/// [`NodeId`]. Errors on wrong length or non-hex input. `pub` for the same
/// reason as [`node_id_hex`].
pub fn node_id_from_hex(s: &str) -> Result<NodeId> {
    if s.len() != 64 {
        return Err(anyhow!("node id hex must be 64 chars, got {}", s.len()));
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|e| anyhow!("invalid node id hex: {e}"))?;
    }
    Ok(out)
}
