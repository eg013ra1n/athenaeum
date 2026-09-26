//! The collab connection pool (collab v3 wave 3, spec §7.3, plan P18).
//!
//! One QUIC connection per provider on
//! [`COLLAB_BLOBS_ALPN`](super::COLLAB_BLOBS_ALPN), kept open while frames are
//! pulled from it and for [`KEEP_OPEN`] after the last request. It is its own
//! pool rather than the stock `iroh_blobs` one because the stock pool dials
//! with plain `connect` and exposes only its idle and connect timeouts; the
//! collab exchange needs a transport config of its own (an explicit
//! [`IDLE_TIMEOUT`] with a [`KEEP_ALIVE`], so a vanished provider is noticed
//! within seconds) and a prompt report when a connection closes. The
//! personal-sync pools keep their own settings.
//!
//! - **Dial.** `connect_with_opts` under a [`CONNECT_TIMEOUT`] that covers
//!   address lookup, relay and handshake. One dial per provider at a time: a
//!   second caller waits for the first and reuses its connection.
//! - **Close.** Every pooled connection has one watcher task. When
//!   `Connection::closed()` fires — the remote went away, the keep-alive gave
//!   up, the idle reaper or [`CollabPool::close`] closed it — the watcher
//!   evicts the entry (if it still holds that connection) and sends exactly
//!   one [`PoolEvent::Closed`]. The scheduler reads that as "this provider is
//!   gone from every in-flight fetch" (I5).
//! - **Idle.** The same task closes a connection nobody holds once
//!   [`KEEP_OPEN`] has passed since its last use.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};

/// Spec §7.3: the dial deadline, covering address lookup, relay and handshake.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Spec §7.3: a connection stays open at least this long after its last request.
pub const KEEP_OPEN: Duration = Duration::from_secs(60);

/// Spec §7.3: the QUIC idle timeout (the iroh default, set explicitly).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Spec §7.3: the QUIC keep-alive interval (the iroh default, set explicitly).
pub const KEEP_ALIVE: Duration = Duration::from_secs(5);

/// What the pool tells its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolEvent {
    /// A pooled connection to `node` closed, for whatever reason (rendered
    /// from the connection's close reason). Sent once per connection.
    Closed { node: EndpointId, reason: String },
}

/// Why [`CollabPool::get`] produced no connection.
#[derive(Debug)]
pub enum DialError {
    /// No connection within the pool's connect timeout.
    Timeout,
    /// The dial or the handshake failed.
    Connect(String),
}

impl std::fmt::Display for DialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "connect timed out"),
            Self::Connect(e) => write!(f, "connect failed: {e}"),
        }
    }
}

/// The transport config every collab dial uses.
fn transport_config() -> iroh::endpoint::QuicTransportConfig {
    iroh::endpoint::QuicTransportConfig::builder()
        .keep_alive_interval(KEEP_ALIVE)
        .max_idle_timeout(Some(
            IDLE_TIMEOUT
                .try_into()
                .expect("30 s is a valid QUIC idle timeout"),
        ))
        .build()
}

/// Lock a pool mutex, reading through a poisoned one (logged): the maps it
/// guards hold no invariant a panicking writer could have half-applied.
fn lock<'a, T>(m: &'a Mutex<T>, what: &'static str) -> MutexGuard<'a, T> {
    m.lock().unwrap_or_else(|poisoned| {
        tracing::error!(
            error = %format!("{what} lock poisoned"),
            "collab pool mutex poisoned; reading through it"
        );
        poisoned.into_inner()
    })
}

/// One pooled connection.
struct Entry {
    conn: Connection,
    last_used: Arc<Mutex<Instant>>,
    /// [`PooledConn`]s handed out and not yet dropped.
    active: Arc<AtomicUsize>,
}

/// One QUIC connection per provider for the collab exchange (module doc).
pub struct CollabPool {
    endpoint: Endpoint,
    entries: Mutex<HashMap<EndpointId, Entry>>,
    /// One dial per provider at a time.
    dial_locks: Mutex<HashMap<EndpointId, Arc<tokio::sync::Mutex<()>>>>,
    events: tokio::sync::mpsc::UnboundedSender<PoolEvent>,
    /// Real dials made (pooled reuse keeps this flat).
    dials: AtomicU64,
    connect_timeout: Duration,
}

/// A borrowed pooled connection. While any is alive the idle reaper leaves
/// the connection open; dropping it marks the connection used now.
pub struct PooledConn {
    pub conn: Connection,
    last_used: Arc<Mutex<Instant>>,
    active: Arc<AtomicUsize>,
}

impl PooledConn {
    fn new(conn: Connection, last_used: Arc<Mutex<Instant>>, active: Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, Ordering::AcqRel);
        *lock(&last_used, "last_used") = Instant::now();
        Self {
            conn,
            last_used,
            active,
        }
    }
}

impl Drop for PooledConn {
    fn drop(&mut self) {
        *lock(&self.last_used, "last_used") = Instant::now();
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

impl CollabPool {
    pub fn new(
        endpoint: Endpoint,
        events: tokio::sync::mpsc::UnboundedSender<PoolEvent>,
    ) -> Arc<Self> {
        Self::build(endpoint, events, CONNECT_TIMEOUT)
    }

    /// A pool with a shorter connect timeout, so a test of an unreachable
    /// provider does not wait out the production one.
    #[cfg(test)]
    pub fn with_connect_timeout(
        endpoint: Endpoint,
        events: tokio::sync::mpsc::UnboundedSender<PoolEvent>,
        t: Duration,
    ) -> Arc<Self> {
        Self::build(endpoint, events, t)
    }

    fn build(
        endpoint: Endpoint,
        events: tokio::sync::mpsc::UnboundedSender<PoolEvent>,
        connect_timeout: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            endpoint,
            entries: Mutex::new(HashMap::new()),
            dial_locks: Mutex::new(HashMap::new()),
            events,
            dials: AtomicU64::new(0),
            connect_timeout,
        })
    }

    /// A connection to `addr.id`: the pooled one while it is open, else a
    /// fresh dial (one at a time per provider).
    pub async fn get(self: &Arc<Self>, addr: EndpointAddr) -> Result<PooledConn, DialError> {
        let node = addr.id;
        if let Some(c) = self.live(&node) {
            return Ok(c);
        }
        let dial_lock = self.dial_lock(&node);
        let _dialing = dial_lock.lock().await;
        if let Some(c) = self.live(&node) {
            return Ok(c);
        }
        let started = Instant::now();
        let opts = iroh::endpoint::ConnectOptions::new().with_transport_config(transport_config());
        let dial = async {
            let connecting = self
                .endpoint
                .connect_with_opts(addr, super::COLLAB_BLOBS_ALPN, opts)
                .await
                .map_err(|e| DialError::Connect(e.to_string()))?;
            connecting
                .await
                .map_err(|e| DialError::Connect(e.to_string()))
        };
        let conn = match tokio::time::timeout(self.connect_timeout, dial).await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                tracing::warn!(
                    node = %node.fmt_short(),
                    error = %e,
                    duration_ms = started.elapsed().as_millis() as u64,
                    "collab dial failed"
                );
                return Err(e);
            }
            Err(_) => {
                let e = DialError::Timeout;
                tracing::warn!(
                    node = %node.fmt_short(),
                    error = %e,
                    duration_ms = started.elapsed().as_millis() as u64,
                    "collab dial failed"
                );
                return Err(e);
            }
        };
        self.dials.fetch_add(1, Ordering::Relaxed);
        super::spawn_conn_path_diagnostics(&conn, "outgoing");
        let last_used = Arc::new(Mutex::new(Instant::now()));
        let active = Arc::new(AtomicUsize::new(0));
        let handed = PooledConn::new(conn.clone(), Arc::clone(&last_used), Arc::clone(&active));
        lock(&self.entries, "entries").insert(
            node,
            Entry {
                conn: conn.clone(),
                last_used: Arc::clone(&last_used),
                active: Arc::clone(&active),
            },
        );
        self.spawn_watcher(node, conn, last_used, active);
        Ok(handed)
    }

    /// Close the pooled connection to `node` (if any) with `reason` and evict
    /// it at once; its watcher sends the [`PoolEvent::Closed`].
    pub fn close(&self, node: &EndpointId, reason: &[u8]) {
        let removed = lock(&self.entries, "entries").remove(node);
        if let Some(entry) = removed {
            entry.conn.close(0u32.into(), reason);
            tracing::info!(
                node = %node.fmt_short(),
                reason = %String::from_utf8_lossy(reason),
                "collab connection closed"
            );
        }
    }

    /// Close and evict every pooled connection whose provider `drop_if`
    /// selects (I11: a member no longer admitted); returns how many.
    pub fn close_where(&self, drop_if: impl Fn(&EndpointId) -> bool) -> usize {
        let closed: Vec<(EndpointId, Entry)> = {
            let mut entries = lock(&self.entries, "entries");
            let ids: Vec<EndpointId> = entries.keys().filter(|id| drop_if(id)).copied().collect();
            ids.into_iter()
                .filter_map(|id| entries.remove(&id).map(|e| (id, e)))
                .collect()
        };
        for (node, entry) in &closed {
            entry.conn.close(0u32.into(), b"not admitted");
            tracing::info!(
                node = %node.fmt_short(),
                reason = "not admitted",
                "collab connection closed"
            );
        }
        closed.len()
    }

    /// Real dials made so far.
    pub fn dials(&self) -> u64 {
        self.dials.load(Ordering::Relaxed)
    }

    /// The pooled connection to `node` if it is still open.
    fn live(&self, node: &EndpointId) -> Option<PooledConn> {
        let entries = lock(&self.entries, "entries");
        let entry = entries.get(node)?;
        if entry.conn.close_reason().is_some() {
            return None;
        }
        Some(PooledConn::new(
            entry.conn.clone(),
            Arc::clone(&entry.last_used),
            Arc::clone(&entry.active),
        ))
    }

    fn dial_lock(&self, node: &EndpointId) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = lock(&self.dial_locks, "dial_locks");
        // Prune locks nobody holds or waits on, so the map tracks the
        // providers being dialled now, not every provider ever seen.
        locks.retain(|id, l| id == node || Arc::strong_count(l) > 1);
        Arc::clone(locks.entry(*node).or_default())
    }

    /// The one task per pooled connection: report its close, and close it
    /// once idle for [`KEEP_OPEN`]. Holds only a `Weak` to the pool.
    fn spawn_watcher(
        self: &Arc<Self>,
        node: EndpointId,
        conn: Connection,
        last_used: Arc<Mutex<Instant>>,
        active: Arc<AtomicUsize>,
    ) {
        let pool: Weak<Self> = Arc::downgrade(self);
        let events = self.events.clone();
        tokio::spawn(async move {
            let stable_id = conn.stable_id();
            let mut idle_closed = false;
            let reason = loop {
                // While a caller holds the connection its drop will touch
                // `last_used`, so look again a full KEEP_OPEN from now rather
                // than at a deadline that may already be in the past.
                let deadline = if active.load(Ordering::Acquire) > 0 {
                    Instant::now() + KEEP_OPEN
                } else {
                    *lock(&last_used, "last_used") + KEEP_OPEN
                };
                tokio::select! {
                    reason = conn.closed() => break reason,
                    _ = tokio::time::sleep_until(deadline.into()), if !idle_closed => {
                        let idle = active.load(Ordering::Acquire) == 0
                            && lock(&last_used, "last_used").elapsed() >= KEEP_OPEN;
                        if idle {
                            if let Some(pool) = pool.upgrade() {
                                pool.evict_if(&node, stable_id);
                            }
                            conn.close(0u32.into(), b"idle");
                            tracing::debug!(node = %node.fmt_short(), "collab connection closed after idle");
                            idle_closed = true;
                        }
                    }
                }
            };
            if let Some(pool) = pool.upgrade() {
                pool.evict_if(&node, stable_id);
            }
            let reason = reason.to_string();
            tracing::debug!(node = %node.fmt_short(), reason = %reason, "collab connection gone");
            if events.send(PoolEvent::Closed { node, reason }).is_err() {
                tracing::debug!(node = %node.fmt_short(), "collab pool event dropped: nobody is listening");
            }
        });
    }

    /// Remove `node`'s entry only if it still holds the connection
    /// `stable_id` — never a newer re-dial.
    fn evict_if(&self, node: &EndpointId, stable_id: usize) {
        let mut entries = lock(&self.entries, "entries");
        if entries.get(node).map(|e| e.conn.stable_id()) == Some(stable_id) {
            entries.remove(node);
        }
    }
}

#[cfg(all(test, feature = "render", feature = "solver"))]
mod tests {
    use super::*;
    use crate::api::collab_live::test_support as ts;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_connection_per_provider_reused_and_a_close_is_reported_at_once() {
        let rig = ts::landed_rig(1).await;
        let me = ts::bare_node().await;
        ts::pair(&me, &rig.node).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = CollabPool::new(me.endpoint(), tx);
        let a = pool.get(rig.node.endpoint_addr()).await.unwrap();
        let b = pool.get(rig.node.endpoint_addr()).await.unwrap();
        assert_eq!(a.conn.stable_id(), b.conn.stable_id());
        assert_eq!(pool.dials(), 1);
        drop((a, b));
        rig.node.shutdown().await;
        let ev = tokio::time::timeout(Duration::from_secs(40), rx.recv())
            .await
            .expect("closed() fires")
            .unwrap();
        assert!(
            matches!(ev, PoolEvent::Closed { node, .. } if node == rig.node.endpoint_addr().id)
        );
    }

    #[tokio::test]
    async fn an_unreachable_provider_times_out() {
        let me = ts::bare_node().await;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = CollabPool::with_connect_timeout(me.endpoint(), tx, Duration::from_millis(500));
        let nobody = iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[42u8; 32]).public());
        assert!(matches!(
            pool.get(nobody).await,
            Err(DialError::Timeout) | Err(DialError::Connect(_))
        ));
    }

    /// An explicit close evicts at once (the next get re-dials) and is
    /// reported exactly once; `close_where` counts what it closed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_explicit_close_is_reported_once_and_the_next_get_redials() {
        let rig = ts::landed_rig(1).await;
        let me = ts::bare_node().await;
        ts::pair(&me, &rig.node).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let pool = CollabPool::new(me.endpoint(), tx);
        let provider = rig.node.endpoint_addr().id;

        let first = pool.get(rig.node.endpoint_addr()).await.unwrap();
        let first_id = first.conn.stable_id();
        drop(first);
        pool.close(&provider, b"test");
        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("the close is reported")
            .unwrap();
        assert!(matches!(ev, PoolEvent::Closed { node, .. } if node == provider));

        let second = pool.get(rig.node.endpoint_addr()).await.unwrap();
        assert_ne!(second.conn.stable_id(), first_id, "a fresh connection");
        assert_eq!(pool.dials(), 2);
        drop(second);

        assert_eq!(pool.close_where(|id| *id != provider), 0);
        assert_eq!(pool.close_where(|id| *id == provider), 1);
        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("the second close is reported")
            .unwrap();
        assert!(matches!(ev, PoolEvent::Closed { node, .. } if node == provider));
        assert!(
            tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .is_err(),
            "one event per closed connection, never two"
        );
    }
}
