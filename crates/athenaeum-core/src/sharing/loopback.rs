//! In-process mock transport for exercising the sync engine without a network.
//!
//! [`LoopbackNetwork::new`] creates a shared peer registry; each
//! [`endpoint`](LoopbackNetwork::endpoint) mints a linked [`LoopbackTransport`]
//! that routes announcements and acks to its peers in-memory and satisfies
//! `fetch` with a plain filesystem copy. A per-endpoint [`FaultPlan`] injects
//! the failure modes the engine's resilience paths need to be tested against.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use super::types::{FrameReceipt, NodeId, PackageAnnounce, PackageId, StartInfo, TransportEvent};
use super::SharingTransport;

/// Per-endpoint fault injection knobs.
///
/// - `abort_after_bytes`: one-shot — the next `fetch` fails once it has copied
///   at least this many bytes, then disarms so a subsequent fetch succeeds.
/// - `duplicate_ack`: the next `ack` delivers its event twice.
/// - `delay_ack`: sleep this long before delivering an ack.
#[derive(Clone, Debug, Default)]
pub struct FaultPlan {
    pub abort_after_bytes: Option<u64>,
    pub duplicate_ack: bool,
    pub delay_ack: Option<Duration>,
}

/// Channel depth for an endpoint's event stream. Announce/ack are low-volume
/// (blocking `send`); fetch progress is best-effort (`try_send`, dropped if
/// full), so this need only comfortably hold pending control events.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Copy granularity for `fetch`; small enough that `abort_after_bytes` can fire
/// partway through a single file.
const COPY_CHUNK_BYTES: usize = 8 * 1024;

/// One peer's mailbox in the shared registry: where to deliver its inbound
/// events, and which packages it is serving (`package_id` → source directory).
struct PeerInbox {
    event_tx: mpsc::Sender<TransportEvent>,
    served: HashMap<String, PathBuf>,
}

/// The in-process network shared by every endpoint minted from it.
type Registry = Arc<Mutex<HashMap<NodeId, PeerInbox>>>;

/// A shared in-process network. Clone-free: hand out endpoints, not copies.
pub struct LoopbackNetwork {
    registry: Registry,
}

impl LoopbackNetwork {
    /// Create an empty network. Endpoints minted from it share one registry.
    pub fn new() -> Self {
        Self {
            registry: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Mint a fresh, unstarted endpoint linked to this network. Call
    /// [`start`](SharingTransport::start) to bring it online (register its
    /// mailbox) before peers announce or ack to it.
    pub fn endpoint(&self) -> LoopbackTransport {
        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        LoopbackTransport {
            node_id: mint_node_id(),
            registry: Arc::clone(&self.registry),
            event_tx,
            event_rx: Mutex::new(Some(event_rx)),
            fault: Mutex::new(FaultPlan::default()),
        }
    }
}

impl Default for LoopbackNetwork {
    fn default() -> Self {
        Self::new()
    }
}

/// One endpoint on a [`LoopbackNetwork`].
pub struct LoopbackTransport {
    node_id: NodeId,
    registry: Registry,
    /// Retained sender for this endpoint's own event stream; also cloned into
    /// the registry so peers can deliver announce/ack events here.
    event_tx: mpsc::Sender<TransportEvent>,
    /// Handed out once by `events()`; `None` afterwards (single-consumer).
    event_rx: Mutex<Option<mpsc::Receiver<TransportEvent>>>,
    fault: Mutex<FaultPlan>,
}

impl LoopbackTransport {
    /// Replace this endpoint's fault plan (test-only knob).
    pub fn set_fault(&self, plan: FaultPlan) {
        *self.fault.lock().expect("fault mutex poisoned") = plan;
    }

    /// Clone the event sender for peer `to`, if that peer has started.
    fn peer_tx(&self, to: NodeId) -> anyhow::Result<mpsc::Sender<TransportEvent>> {
        let reg = self.registry.lock().expect("registry mutex poisoned");
        reg.get(&to)
            .map(|inbox| inbox.event_tx.clone())
            .ok_or_else(|| anyhow!("peer not started: {}", hex32(&to)))
    }
}

#[async_trait]
impl SharingTransport for LoopbackTransport {
    async fn start(&self) -> anyhow::Result<StartInfo> {
        {
            let mut reg = self.registry.lock().expect("registry mutex poisoned");
            reg.entry(self.node_id).or_insert_with(|| PeerInbox {
                event_tx: self.event_tx.clone(),
                served: HashMap::new(),
            });
        }
        let pairing_ticket = pairing_ticket(&self.node_id);
        tracing::debug!(node_id = %hex32(&self.node_id), "loopback endpoint started");
        Ok(StartInfo {
            node_id: self.node_id,
            pairing_ticket,
        })
    }

    async fn announce(&self, to: NodeId, a: &PackageAnnounce) -> anyhow::Result<()> {
        let tx = self.peer_tx(to)?;
        tx.send(TransportEvent::AnnounceReceived {
            from: self.node_id,
            announce: a.clone(),
        })
        .await
        .map_err(|_| anyhow!("peer event channel closed: {}", hex32(&to)))?;
        tracing::debug!(
            to = %hex32(&to),
            package_id = %a.package_id.0,
            "loopback announce delivered"
        );
        Ok(())
    }

    async fn fetch(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
    ) -> anyhow::Result<()> {
        // Resolve the provider's served source directory.
        let src_dir = {
            let reg = self.registry.lock().expect("registry mutex poisoned");
            reg.get(&from)
                .and_then(|inbox| inbox.served.get(&pkg.package_id.0).cloned())
        }
        .ok_or_else(|| {
            anyhow!(
                "package not served by peer: package_id={} from={}",
                pkg.package_id.0,
                hex32(&from)
            )
        })?;

        let files = collect_files(&src_dir)?;
        let bytes_total: u64 = files.iter().map(|f| f.size).sum();
        // Read the one-shot abort threshold once; disarm inside the loop if it fires.
        let abort_after = self.fault.lock().expect("fault mutex poisoned").abort_after_bytes;

        tokio::fs::create_dir_all(dest_dir)
            .await
            .with_context(|| format!("create dest dir {}", dest_dir.display()))?;

        let mut bytes_done: u64 = 0;
        for file in &files {
            let dest_path = dest_dir.join(&file.rel);
            if let Some(parent) = dest_path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("create dir {}", parent.display()))?;
            }
            let mut reader = tokio::fs::File::open(&file.abs)
                .await
                .with_context(|| format!("open source {}", file.abs.display()))?;
            let mut writer = tokio::fs::File::create(&dest_path)
                .await
                .with_context(|| format!("create dest {}", dest_path.display()))?;

            let mut buf = vec![0u8; COPY_CHUNK_BYTES];
            loop {
                let n = reader.read(&mut buf).await.context("read chunk")?;
                if n == 0 {
                    break;
                }
                writer.write_all(&buf[..n]).await.context("write chunk")?;
                bytes_done += n as u64;

                // Progress is UI data, not control flow: best-effort, never blocks.
                let _ = self.event_tx.try_send(TransportEvent::FetchProgress {
                    package_id: pkg.package_id.clone(),
                    bytes_done,
                    bytes_total,
                });

                if let Some(threshold) = abort_after {
                    if bytes_done >= threshold {
                        writer.flush().await.ok();
                        // One-shot: disarm so the next fetch completes.
                        self.fault.lock().expect("fault mutex poisoned").abort_after_bytes = None;
                        tracing::warn!(
                            from = %hex32(&from),
                            package_id = %pkg.package_id.0,
                            bytes = bytes_done,
                            "loopback fetch aborted (injected fault)"
                        );
                        return Err(anyhow!(
                            "injected fault: fetch aborted after {bytes_done} bytes"
                        ));
                    }
                }
            }
            writer.flush().await.context("flush dest")?;
        }

        tracing::debug!(
            from = %hex32(&from),
            package_id = %pkg.package_id.0,
            bytes = bytes_done,
            count = files.len(),
            "loopback fetch complete"
        );
        Ok(())
    }

    async fn serve(&self, pkg: &PackageAnnounce, src_dir: &Path) -> anyhow::Result<()> {
        let mut reg = self.registry.lock().expect("registry mutex poisoned");
        let inbox = reg
            .get_mut(&self.node_id)
            .ok_or_else(|| anyhow!("endpoint not started"))?;
        inbox
            .served
            .insert(pkg.package_id.0.clone(), src_dir.to_path_buf());
        tracing::debug!(
            package_id = %pkg.package_id.0,
            path = %src_dir.display(),
            "loopback serving package"
        );
        Ok(())
    }

    async fn ack(
        &self,
        to: NodeId,
        package_id: &PackageId,
        receipts: Vec<FrameReceipt>,
    ) -> anyhow::Result<()> {
        let tx = self.peer_tx(to)?;

        // Read fault knobs up front; do not hold the lock across await.
        let (delay, duplicate) = {
            let fault = self.fault.lock().expect("fault mutex poisoned");
            (fault.delay_ack, fault.duplicate_ack)
        };
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }

        let deliveries = if duplicate { 2 } else { 1 };
        for _ in 0..deliveries {
            tx.send(TransportEvent::AckReceived {
                from: self.node_id,
                package_id: package_id.clone(),
                receipts: receipts.clone(),
            })
            .await
            .map_err(|_| anyhow!("peer event channel closed: {}", hex32(&to)))?;
        }
        tracing::debug!(
            to = %hex32(&to),
            package_id = %package_id.0,
            count = deliveries,
            "loopback ack delivered"
        );
        Ok(())
    }

    async fn events(&self) -> mpsc::Receiver<TransportEvent> {
        let mut guard = self.event_rx.lock().expect("event_rx mutex poisoned");
        match guard.take() {
            Some(rx) => rx,
            None => {
                // Single-consumer: subsequent calls get an already-closed receiver.
                let (_tx, rx) = mpsc::channel(1);
                rx
            }
        }
    }
}

/// A regular file discovered under a served directory.
struct SrcFile {
    abs: PathBuf,
    rel: PathBuf,
    size: u64,
}

/// Recursively list regular files under `src`, sorted by relative path so copy
/// order (and thus `abort_after_bytes`) is deterministic.
fn collect_files(src: &Path) -> anyhow::Result<Vec<SrcFile>> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(src).sort_by_file_name() {
        let entry = entry.with_context(|| format!("walk {}", src.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let abs = entry.path().to_path_buf();
        let rel = abs
            .strip_prefix(src)
            .with_context(|| format!("strip prefix {}", src.display()))?
            .to_path_buf();
        let size = entry.metadata().context("stat file")?.len();
        out.push(SrcFile { abs, rel, size });
    }
    Ok(out)
}

/// Mint a fresh 32-byte node id (two uuid-v4s concatenated — no extra deps).
fn mint_node_id() -> NodeId {
    let mut id = [0u8; 32];
    id[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    id[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    id
}

/// Opaque pairing ticket for the mock: base64 of the node id.
fn pairing_ticket(node_id: &NodeId) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(node_id)
}

/// Short hex rendering of a node id for log fields.
fn hex32(node_id: &NodeId) -> String {
    node_id.iter().map(|b| format!("{b:02x}")).collect()
}
