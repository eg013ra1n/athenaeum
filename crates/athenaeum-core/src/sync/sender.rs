//! App-side sender runtime holder (Stage I, task M2).
//!
//! The capture-role counterpart of [`SyncRuntime`](super::receiver::SyncRuntime):
//! where the receiver holds a running [`SyncReceiver`](super::receiver::SyncReceiver),
//! this holds a running sender-side [`SyncEngine`](super::engine::SyncEngine).
//! It lives in the host `AppState` (desktop + web) and is reached by the send
//! commands in [`crate::api::sync`].
//!
//! It is a **dumb holder** on purpose — the orchestration that resolves the peer
//! + relays and builds the iroh transport lives in [`crate::api::sync`] (which
//! owns the account/device plumbing), exactly like [`SyncRuntime`] is driven by
//! `api::sync::get_pairing_ticket`. Each engine is constructed lazily on the
//! first enqueue to a given destination
//! ([`ensure_sender_engine`](crate::api::sync::ensure_sender_engine)) and cached
//! here for the process lifetime.
//!
//! # Per-peer map (sync 2C)
//!
//! Explicit-target send addresses each destination device by its [`NodeId`], so
//! the runtime holds **one engine per peer** (`HashMap<NodeId, StartedSender>`)
//! rather than a single slot. A device can therefore send to several peers
//! concurrently, each over its own engine/transport. Holding the
//! [`tokio::sync::Mutex`] across the (async) transport build in the ensure path
//! is what guarantees exactly one engine per peer is ever spawned — a second
//! concurrent enqueue to the same peer blocks on the lock and then sees the
//! populated entry.

use std::collections::HashMap;
use std::sync::Arc;

use crate::sharing::types::NodeId;

use super::engine::SyncEngineHandle;

/// One started sender bundle held by [`SyncSenderRuntime`].
pub struct StartedSender {
    /// The running engine the send commands enqueue packages into.
    pub engine: Arc<SyncEngineHandle>,
    /// This device's own node id (hex) — stamped into each manifest record's
    /// `origin_device` so the receiver can attribute the frames.
    pub origin_device: String,
    /// The resolved peer this engine sends to. Retained for status/diagnostics;
    /// the engine itself already owns it.
    pub peer: NodeId,
}

/// App-lifecycle holder for the send side. Cheap to construct; each peer's
/// engine + transport are built lazily on the first enqueue to that peer, keyed
/// by the destination [`NodeId`].
pub struct SyncSenderRuntime {
    inner: tokio::sync::Mutex<HashMap<NodeId, StartedSender>>,
}

impl SyncSenderRuntime {
    /// A fresh runtime with no started engines.
    pub fn new() -> Self {
        Self {
            inner: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Whether at least one peer engine has been started.
    pub async fn is_started(&self) -> bool {
        !self.inner.lock().await.is_empty()
    }

    /// The running engine handle + this device's origin id for `peer`, if an
    /// engine to that peer has been started.
    pub async fn current_for(&self, peer: &NodeId) -> Option<(Arc<SyncEngineHandle>, String)> {
        self.inner
            .lock()
            .await
            .get(peer)
            .map(|s| (Arc::clone(&s.engine), s.origin_device.clone()))
    }

    /// The destination peers with a started engine (order unspecified).
    pub async fn started_peers(&self) -> Vec<NodeId> {
        self.inner.lock().await.keys().copied().collect()
    }

    /// Wake **every** started peer engine (send-now / relay-online nudge, spec
    /// §2, Task 5): each engine collapses its packages' backoff deadlines so a
    /// retry fires on the next worker pass. Fire-and-forget and log-and-continue
    /// — the engine handles are snapshotted under the lock (released before any
    /// await), then each is kicked on its own detached task so one slow or
    /// stopped engine can never block the others or the caller. Order across
    /// peers is unspecified. Tasks 7/9 call this on an authorized-peers refresh.
    pub async fn kick_all(&self) {
        let engines: Vec<Arc<SyncEngineHandle>> = {
            let inner = self.inner.lock().await;
            inner.values().map(|s| Arc::clone(&s.engine)).collect()
        };
        for engine in engines {
            tokio::spawn(async move {
                if let Err(e) = engine.kick_all().await {
                    tracing::warn!(error = %e, "sync sender kick_all: engine kick failed");
                }
            });
        }
    }

    /// Tell ONE peer's engine that its peer just announced itself online (D1),
    /// if an engine for that peer is started.
    ///
    /// Fire-and-forget and log-and-continue, like [`kick_all`](Self::kick_all).
    /// This deliberately does NOT build an engine: whether there is anything worth
    /// resuming for that peer is a question about durable rows and the account
    /// allow-list, which this dumb holder cannot answer —
    /// `api::sync::handle_peer_presence` gates that and builds the engine first.
    pub async fn kick_peer(&self, peer: &NodeId) {
        let engine = self
            .inner
            .lock()
            .await
            .get(peer)
            .map(|s| Arc::clone(&s.engine));
        match engine {
            Some(engine) => {
                if let Err(e) = engine.peer_present().await {
                    tracing::warn!(
                        error = %e,
                        peer = %crate::sync::node_id_hex(peer),
                        "peer-present kick failed"
                    );
                }
            }
            None => tracing::debug!(
                peer = %crate::sync::node_id_hex(peer),
                "peer-present kick: no engine for this peer"
            ),
        }
    }

    /// Lock the per-peer map for the ensure critical section. The orchestration
    /// in [`crate::api::sync::ensure_sender_engine`] holds this guard across the
    /// transport build so a second concurrent enqueue to the SAME peer can never
    /// spawn a second engine for it. Tests inject loopback-backed engines by
    /// inserting into this map directly.
    pub async fn lock_inner(&self) -> tokio::sync::MutexGuard<'_, HashMap<NodeId, StartedSender>> {
        self.inner.lock().await
    }
}

impl Default for SyncSenderRuntime {
    fn default() -> Self {
        Self::new()
    }
}
