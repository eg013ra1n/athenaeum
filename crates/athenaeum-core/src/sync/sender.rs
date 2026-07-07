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
//! owns the account/pairing plumbing), exactly like [`SyncRuntime`] is driven by
//! `api::sync::get_pairing_ticket`. The engine is constructed lazily on the first
//! enqueue ([`ensure_sender_engine`](crate::api::sync::ensure_sender_engine)) and
//! cached here for the process lifetime.
//!
//! Holding the [`tokio::sync::Mutex`] across the (async) transport build in the
//! ensure path is what guarantees exactly one engine is ever spawned — a second
//! concurrent enqueue blocks on the lock and then sees the populated slot.

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

/// App-lifecycle holder for the send side. Cheap to construct; the engine +
/// transport are built lazily on the first enqueue.
pub struct SyncSenderRuntime {
    inner: tokio::sync::Mutex<Option<StartedSender>>,
}

impl SyncSenderRuntime {
    /// A fresh, unstarted runtime.
    pub fn new() -> Self {
        Self {
            inner: tokio::sync::Mutex::new(None),
        }
    }

    /// Whether the engine has been started.
    pub async fn is_started(&self) -> bool {
        self.inner.lock().await.is_some()
    }

    /// The running engine handle + this device's origin id, if started.
    pub async fn current(&self) -> Option<(Arc<SyncEngineHandle>, String)> {
        self.inner
            .lock()
            .await
            .as_ref()
            .map(|s| (Arc::clone(&s.engine), s.origin_device.clone()))
    }

    /// Lock the inner slot for the ensure critical section. The orchestration in
    /// [`crate::api::sync::ensure_sender_engine`] holds this guard across the
    /// transport build so a second concurrent enqueue can never spawn a second
    /// engine. Tests inject a loopback-backed engine by setting this slot
    /// directly.
    pub async fn lock_inner(&self) -> tokio::sync::MutexGuard<'_, Option<StartedSender>> {
        self.inner.lock().await
    }
}

impl Default for SyncSenderRuntime {
    fn default() -> Self {
        Self::new()
    }
}
