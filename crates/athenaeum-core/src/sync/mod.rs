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
//!   once as consts) and the [`StandaloneSyncStore`](store::StandaloneSyncStore)
//!   rusqlite implementation (own WAL SQLite file). The catalog-backed
//!   implementation (`CatalogSyncStore`) is deliberately deferred to task A7 so
//!   this task never touches `db/schema.rs`.
//! - [`engine`] — [`SyncEngine`](engine::SyncEngine) / its
//!   [`SyncEngineHandle`](engine::SyncEngineHandle) and the tokio worker task
//!   that implements the state machine.
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

pub mod engine;
pub mod models;
pub mod store;

#[cfg(test)]
mod engine_tests;

pub use engine::{SyncConfig, SyncEngine, SyncEngineHandle, DEFAULT_ACK_TIMEOUT, MAX_ATTEMPTS};
pub use models::{Direction, HistoryQuery, HistoryRow, OutboundRow, OutboundState};
pub use store::{StandaloneSyncStore, SyncStore};

/// Canonical timestamp rendering for the sync tables: RFC3339 UTC, millisecond
/// precision, `Z` suffix. Sortable as text, unambiguous across time zones.
pub(crate) fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Lowercase-hex rendering of a 32-byte node id (64 chars), used as the stored
/// `peer` / `peer_device` column value.
pub(crate) fn node_id_hex(id: &NodeId) -> String {
    let mut s = String::with_capacity(64);
    for b in id {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Parse the 64-char lowercase-hex form produced by [`node_id_hex`] back into a
/// [`NodeId`]. Errors on wrong length or non-hex input.
pub(crate) fn node_id_from_hex(s: &str) -> Result<NodeId> {
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
