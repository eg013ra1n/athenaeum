//! In-process mock transport for exercising the sync engine without a network.
//!
//! [`LoopbackNetwork::new`] creates a shared peer registry; each
//! [`endpoint`](LoopbackNetwork::endpoint) mints a linked [`LoopbackTransport`]
//! that routes announcements and acks to its peers in-memory and satisfies
//! `fetch` with a plain filesystem copy. A per-endpoint [`FaultPlan`] injects
//! the failure modes the engine's resilience paths need to be tested against.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use super::iroh::proto::OfferEntry;
use super::types::{
    AnnounceFileEntry, FetchEvent, FrameReceipt, NodeId, PackageAnnounce, PackageId, RevokeReason,
    StartInfo, TransportEvent,
};
use super::{FetchSink, SharingTransport};
use crate::package::MANIFEST_FILENAME;
use crate::sync::DedupResponder;

/// Per-endpoint fault injection knobs.
///
/// - `abort_after_bytes`: one-shot — the next `fetch` fails once it has copied
///   at least this many bytes, then disarms so a subsequent fetch succeeds.
/// - `duplicate_ack`: the next `ack` delivers its event twice.
/// - `delay_ack`: sleep this long before delivering an ack.
/// - `delay_per_read`: sleep this long after every fetch read chunk, so a whole
///   `fetch` takes a controllable, non-trivial wall-clock duration. Test-only,
///   additive, off by default — the abort knob can only *fail* a fetch, so this
///   is what lets a test make two concurrent fetches genuinely overlap (the
///   bidirectional e2e's non-serialization proof).
#[derive(Clone, Debug, Default)]
pub struct FaultPlan {
    pub abort_after_bytes: Option<u64>,
    pub duplicate_ack: bool,
    pub delay_ack: Option<Duration>,
    pub delay_per_read: Option<Duration>,
}

/// Channel depth for an endpoint's event stream. Announce/ack are low-volume
/// (blocking `send`), so this need only comfortably hold pending control events.
/// Fetch progress does not ride this channel — it goes through the [`FetchSink`]
/// callback threaded into `fetch`.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Copy granularity for `fetch`; small enough that `abort_after_bytes` can fire
/// partway through a single file.
const COPY_CHUNK_BYTES: usize = 8 * 1024;

/// A served package in the mock: its source directory plus the optional
/// negotiated want subset. `want = None` mirrors a full serve; `Some(rel_paths)`
/// mirrors the iroh subset collection — `fetch` transfers only those payloads
/// plus a manifest filtered to them, so the loopback e2e is honest.
#[derive(Clone)]
struct ServedPackage {
    src_dir: PathBuf,
    want: Option<HashSet<String>>,
}

/// One peer's mailbox in the shared registry: where to deliver its inbound
/// events, which packages it is serving (`package_id` → [`ServedPackage`]), and
/// the optional dedup responder that answers inbound `negotiate_want` offers.
struct PeerInbox {
    event_tx: mpsc::Sender<TransportEvent>,
    served: HashMap<String, ServedPackage>,
    /// Answers a peer's dedup handshake (`None` = want-all, no dedup). The
    /// in-process counterpart of the iroh `SyncControlProtocol`'s responder.
    responder: Option<Arc<dyn DedupResponder>>,
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
            responder: None,
        }
    }

    /// Mint an endpoint that answers a peer's dedup handshake via `responder`
    /// (registered into its inbox on [`start`](SharingTransport::start)) — the
    /// in-process stand-in for a running receiver's `CatalogDedupResponder`. Its
    /// send-only counterpart is [`endpoint`](Self::endpoint), whose peers get a
    /// want-all (dedup-free) response.
    pub fn endpoint_with_responder(
        &self,
        responder: Arc<dyn DedupResponder>,
    ) -> LoopbackTransport {
        let mut ep = self.endpoint();
        ep.responder = Some(responder);
        ep
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
    /// Registered into this endpoint's [`PeerInbox`] on `start`; answers dedup
    /// handshakes from peers. `None` for a plain (want-all) endpoint.
    responder: Option<Arc<dyn DedupResponder>>,
}

impl LoopbackTransport {
    /// This endpoint's node id. Available *before* [`start`](SharingTransport::start),
    /// so a test can address a peer that is not yet online (its mailbox is not
    /// registered until `start`), then bring it up later.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

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
                responder: self.responder.clone(),
            });
        }
        let pairing_ticket = pairing_ticket(&self.node_id);
        tracing::debug!(node_id = %hex32(&self.node_id), "loopback endpoint started");
        Ok(StartInfo {
            node_id: self.node_id,
            pairing_ticket,
        })
    }

    async fn announce(
        &self,
        to: NodeId,
        a: &PackageAnnounce,
        batch_name: &str,
        batch_uuid: &str,
        files: &[AnnounceFileEntry],
    ) -> anyhow::Result<()> {
        let tx = self.peer_tx(to)?;
        // The loopback emulates the app sender, which emits only v3 — so the
        // extras always arrive as `Some` (mirroring a `Msg::Announce3` decode) and
        // `batch_uuid` threads straight through, even when a caller passes an empty
        // manifest / basename placeholder.
        tx.send(TransportEvent::AnnounceReceived {
            from: self.node_id,
            announce: a.clone(),
            batch_name: Some(batch_name.to_string()),
            batch_uuid: batch_uuid.to_string(),
            files: Some(files.to_vec()),
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

    async fn revoke(
        &self,
        to: NodeId,
        package_id: &PackageId,
        reason: RevokeReason,
    ) -> anyhow::Result<()> {
        // Mirror the real transports' revoke routing: deliver a `RevokeReceived`
        // to the target peer's inbox, just as `announce` delivers `AnnounceReceived`.
        let tx = self.peer_tx(to)?;
        tx.send(TransportEvent::RevokeReceived {
            from: self.node_id,
            package_id: package_id.clone(),
            reason,
        })
        .await
        .map_err(|_| anyhow!("peer event channel closed: {}", hex32(&to)))?;
        tracing::debug!(
            to = %hex32(&to),
            package_id = %package_id.0,
            ?reason,
            "loopback revoke delivered"
        );
        Ok(())
    }

    async fn announce_project(
        &self,
        to: NodeId,
        project_id: &str,
        package_id: &str,
        a: &PackageAnnounce,
    ) -> anyhow::Result<()> {
        let tx = self.peer_tx(to)?;
        tx.send(TransportEvent::ProjectAnnounceReceived {
            from: self.node_id,
            project_id: project_id.to_string(),
            package_id: package_id.to_string(),
            announce: a.clone(),
        })
        .await
        .map_err(|_| anyhow!("peer event channel closed: {}", hex32(&to)))?;
        tracing::debug!(
            to = %hex32(&to),
            project_id,
            package_id,
            wire_package_id = %a.package_id.0,
            "loopback project announce delivered"
        );
        Ok(())
    }

    async fn request_project(
        &self,
        to: NodeId,
        project_id: &str,
        package_id: &str,
    ) -> anyhow::Result<()> {
        let tx = self.peer_tx(to)?;
        tx.send(TransportEvent::ProjectRequestReceived {
            from: self.node_id,
            project_id: project_id.to_string(),
            package_id: package_id.to_string(),
        })
        .await
        .map_err(|_| anyhow!("peer event channel closed: {}", hex32(&to)))?;
        tracing::debug!(
            to = %hex32(&to),
            project_id,
            package_id,
            "loopback project request delivered"
        );
        Ok(())
    }

    async fn fetch(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
        sink: FetchSink,
    ) -> anyhow::Result<()> {
        // Resolve the provider's served package (source dir + optional want subset).
        let served = {
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
        let ServedPackage { src_dir, want } = served;

        // Synthetic upload progress to the SERVING endpoint's event stream (Task
        // 13): the iroh transport emits `ServeProgress` from real provider upload
        // events as a peer pulls the collection; the mock emits one deterministic
        // tick equal to the full announced byte size, so the sender engine's
        // `ServeProgress` arm is exercised in-process. Pushed here — before the
        // file copy, and so strictly before the receiver's ack — so the sender
        // sees it while the package's pending slot is still live. Best-effort
        // (`try_send`): a full/closed provider channel just drops it (progress is
        // UI data, never load-bearing).
        if let Some(tx) = {
            let reg = self.registry.lock().expect("registry mutex poisoned");
            reg.get(&from).map(|inbox| inbox.event_tx.clone())
        } {
            let _ = tx.try_send(TransportEvent::ServeProgress {
                package_id: pkg.package_id.clone(),
                bytes_sent: pkg.byte_size,
            });
        }

        // Which files move: a full serve transfers every file under `src_dir`
        // (manifest included); a want-subset serve transfers only the wanted
        // payloads and re-synthesizes a manifest filtered to them, so the mock
        // mirrors the iroh subset collection instead of copying the whole dir.
        let files = match &want {
            None => collect_files(&src_dir)?,
            Some(w) => {
                let kept = filter_manifest_records(&src_dir, w)?;
                write_filtered_manifest(dest_dir, &kept).await?;
                subset_src_files(&src_dir, &kept)?
            }
        };
        // Read the one-shot abort threshold + optional per-read pacing once; disarm
        // the abort inside the loop if it fires.
        let (abort_after, delay_per_read) = {
            let f = self.fault.lock().expect("fault mutex poisoned");
            (f.abort_after_bytes, f.delay_per_read)
        };

        tokio::fs::create_dir_all(dest_dir)
            .await
            .with_context(|| format!("create dest dir {}", dest_dir.display()))?;

        // The serving endpoint's event stream, cloned once for the synthetic
        // per-file upload ticks (Task 2.2): the iroh provider attributes each served
        // child to its collection entry by hash-seq index; the mock emits one
        // deterministic `ServeFileProgress` per file, in order, mirroring that
        // contract shape (per-file, ordered) so the sender engine's arm is exercised
        // in-process. Best-effort (`try_send`).
        let provider_tx = {
            let reg = self.registry.lock().expect("registry mutex poisoned");
            reg.get(&from).map(|inbox| inbox.event_tx.clone())
        };

        let mut bytes_done: u64 = 0;
        for file in &files {
            let name = rel_to_name(&file.rel);
            // Synthetic per-file progress: start at 0, then jump to complete after
            // the copy — deterministic, no timing (the iroh transport observes real
            // per-file byte progress; the mock only needs the contract shape).
            sink(FetchEvent::File {
                name: name.clone(),
                bytes_done: 0,
                bytes_total: file.size,
            });
            // Symmetric provider-side START tick (Task 2.2): the iroh provider emits
            // real partial `ServeFileProgress` before a file completes, so the mock
            // emits a `bytes_done: 0` tick here (mirroring the `FetchEvent::File`
            // start above) — this is what lets the sender engine mark a per-file row
            // `sending` before it uploads. Best-effort (`try_send`).
            if let Some(tx) = &provider_tx {
                let _ = tx.try_send(TransportEvent::ServeFileProgress {
                    package_id: pkg.package_id.clone(),
                    file: name.clone(),
                    bytes_done: 0,
                    bytes_total: file.size,
                });
            }

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

                // Test-only pacing: yield and sleep after each read so the fetch
                // takes a controllable duration. `.await` hands the runtime back to
                // any concurrent fetch, so two paced fetches genuinely interleave.
                if let Some(d) = delay_per_read {
                    tokio::time::sleep(d).await;
                }
            }
            writer.flush().await.context("flush dest")?;

            // Synthetic per-file upload tick to the SERVING endpoint (Task 2.2),
            // one per file in order — the mock's counterpart of the iroh consumer's
            // by-index `ServeFileProgress`. Emitted BEFORE the receiver's ack so the
            // sender sees it while the pending slot is live.
            if let Some(tx) = &provider_tx {
                let _ = tx.try_send(TransportEvent::ServeFileProgress {
                    package_id: pkg.package_id.clone(),
                    file: name.clone(),
                    bytes_done: file.size,
                    bytes_total: file.size,
                });
            }

            // File complete: emit the terminal per-file event unconditionally.
            sink(FetchEvent::File {
                name,
                bytes_done: file.size,
                bytes_total: file.size,
            });
        }

        // Final aggregate event reaches the announced package size.
        sink(FetchEvent::Batch {
            bytes_done: pkg.byte_size,
            bytes_total: pkg.byte_size,
        });

        // Synthetic upload-complete to the SERVING endpoint's event stream (Task
        // 2.1): the iroh provider routes `ServeComplete` on the terminal
        // `Completed` of a payload-carrying request; the mock emits one after a
        // successful copy so the sender engine's ServeComplete arm ("uploaded —
        // awaiting confirmation") is exercised in-process. Pushed here — after the
        // copy but BEFORE the receiver's ack (which the engine's reactive receiver
        // fires only after `fetch` returns) — so the sender sees `uploaded`
        // strictly before `confirmed`. Best-effort (`try_send`): a full/closed
        // provider channel just drops it (signalling is UI data, never
        // load-bearing).
        if let Some(tx) = {
            let reg = self.registry.lock().expect("registry mutex poisoned");
            reg.get(&from).map(|inbox| inbox.event_tx.clone())
        } {
            let _ = tx.try_send(TransportEvent::ServeComplete {
                package_id: pkg.package_id.clone(),
            });
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

    async fn fetch_manifest(
        &self,
        from: NodeId,
        pkg: &PackageAnnounce,
        dest_dir: &Path,
    ) -> anyhow::Result<PathBuf> {
        // Resolve the provider's served package source dir, then copy just its
        // `manifest.ndjson` into `dest_dir` — the in-process mirror of the iroh
        // manifest-only fetch (hash-seq + meta + the named manifest blob).
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
        })?
        .src_dir;

        tokio::fs::create_dir_all(dest_dir)
            .await
            .with_context(|| format!("create dest dir {}", dest_dir.display()))?;
        let src = src_dir.join(MANIFEST_FILENAME);
        let dest = dest_dir.join(MANIFEST_FILENAME);
        tokio::fs::copy(&src, &dest)
            .await
            .with_context(|| format!("copy manifest {} -> {}", src.display(), dest.display()))?;
        tracing::debug!(
            from = %hex32(&from),
            package_id = %pkg.package_id.0,
            path = %dest.display(),
            "loopback manifest fetched"
        );
        Ok(dest)
    }

    async fn serve(
        &self,
        pkg: &PackageAnnounce,
        src_dir: &Path,
        want: Option<&HashSet<String>>,
    ) -> anyhow::Result<()> {
        let mut reg = self.registry.lock().expect("registry mutex poisoned");
        let inbox = reg
            .get_mut(&self.node_id)
            .ok_or_else(|| anyhow!("endpoint not started"))?;
        inbox.served.insert(
            pkg.package_id.0.clone(),
            ServedPackage {
                src_dir: src_dir.to_path_buf(),
                want: want.cloned(),
            },
        );
        tracing::debug!(
            package_id = %pkg.package_id.0,
            path = %src_dir.display(),
            subset = want.is_some(),
            "loopback serving package"
        );
        Ok(())
    }

    async fn release(&self, package_id: &PackageId) -> anyhow::Result<()> {
        let mut reg = self.registry.lock().expect("registry mutex poisoned");
        if let Some(inbox) = reg.get_mut(&self.node_id) {
            let removed = inbox.served.remove(&package_id.0).is_some();
            tracing::debug!(package_id = %package_id.0, removed, "loopback released package");
        }
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

    async fn negotiate_want(
        &self,
        to: NodeId,
        _package_id: PackageId,
        offer: Vec<OfferEntry>,
        full_by_rel: HashMap<String, String>,
    ) -> anyhow::Result<HashSet<String>> {
        // Read the target peer's responder (clone the Arc, then drop the lock —
        // the responder calls below must not hold the registry mutex).
        let responder = {
            let reg = self.registry.lock().expect("registry mutex poisoned");
            let inbox = reg
                .get(&to)
                .ok_or_else(|| anyhow!("peer not started: {}", hex32(&to)))?;
            inbox.responder.clone()
        };

        // No responder → want-all (dedup-free), mirroring the iroh accept side.
        let responder = match responder {
            None => return Ok(offer.into_iter().map(|e| e.rel_path).collect()),
            Some(r) => r,
        };

        // Round 1: offer → (definite wants, ambiguous candidates).
        let (want, candidates) = responder.want_for_offer(&offer);
        let mut wanted: HashSet<String> = want.into_iter().collect();
        if candidates.is_empty() {
            return Ok(wanted);
        }

        // Round 2: answer each candidate with its full hash, confirm, union.
        let entries = super::iroh::proto::build_full_hash_entries(
            &offer,
            &candidates,
            &full_by_rel,
            &mut wanted,
        );
        wanted.extend(responder.confirm_full_hashes(&entries));
        Ok(wanted)
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

/// Read the served package's manifest and keep only the wanted records (order
/// preserved) — the mock's counterpart of `blobs::import_subset_collection`'s
/// manifest filter.
fn filter_manifest_records(
    src_dir: &Path,
    want: &HashSet<String>,
) -> anyhow::Result<Vec<crate::package::ManifestRecord>> {
    let records = crate::package::read_manifest(src_dir)?;
    Ok(records
        .into_iter()
        .filter(|r| want.contains(&r.rel_path))
        .collect())
}

/// Write a `manifest.ndjson` holding exactly `kept` into `dest_dir`, in the same
/// compact one-record-per-line format `read_manifest` parses.
async fn write_filtered_manifest(
    dest_dir: &Path,
    kept: &[crate::package::ManifestRecord],
) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(dest_dir)
        .await
        .with_context(|| format!("create dest dir {}", dest_dir.display()))?;
    let mut buf = String::new();
    for r in kept {
        buf.push_str(&serde_json::to_string(r).context("serialize filtered manifest record")?);
        buf.push('\n');
    }
    let path = dest_dir.join(crate::package::MANIFEST_FILENAME);
    tokio::fs::write(&path, buf.as_bytes())
        .await
        .with_context(|| format!("write filtered manifest {}", path.display()))?;
    Ok(())
}

/// The [`SrcFile`] copy list for a want-subset fetch: each kept payload under
/// `src_dir` at its `rel_path` (the manifest is written separately).
fn subset_src_files(
    src_dir: &Path,
    kept: &[crate::package::ManifestRecord],
) -> anyhow::Result<Vec<SrcFile>> {
    let mut out = Vec::with_capacity(kept.len());
    for r in kept {
        let abs = src_dir.join(&r.rel_path);
        let size = std::fs::metadata(&abs)
            .with_context(|| format!("stat wanted payload {}", abs.display()))?
            .len();
        out.push(SrcFile {
            abs,
            rel: PathBuf::from(&r.rel_path),
            size,
        });
    }
    Ok(out)
}

/// Forward-slash collection-entry name for a relative path, matching the iroh
/// transport's entry naming so [`FetchEvent::File`] keys agree across transports.
fn rel_to_name(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
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

#[cfg(test)]
mod negotiate_tests {
    use super::*;
    use crate::sharing::iroh::proto::{FullHashEntry, OfferEntry};
    use crate::sync::DedupResponder;
    use std::collections::{HashMap, HashSet};

    /// A catalog-free [`DedupResponder`] for the loopback handshake test.
    /// `present` are the sampling hashes it "has"; `local_full` maps each such
    /// sampling hash to the full xxh3 of the local file carrying it, so a
    /// full-hash match can be settled without touching disk.
    struct StubResponder {
        present: HashSet<String>,
        local_full: HashMap<String, String>,
    }

    impl DedupResponder for StubResponder {
        fn want_for_offer(&self, entries: &[OfferEntry]) -> (Vec<String>, Vec<String>) {
            let mut want = Vec::new();
            let mut cands = Vec::new();
            for e in entries {
                if self.present.contains(&e.sampling_hash) {
                    cands.push(e.rel_path.clone());
                } else {
                    want.push(e.rel_path.clone());
                }
            }
            (want, cands)
        }

        fn confirm_full_hashes(&self, entries: &[FullHashEntry]) -> Vec<String> {
            entries
                .iter()
                .filter(|e| match self.local_full.get(&e.sampling_hash) {
                    // Same full hash as our local file → true duplicate, drop it.
                    Some(local) => local != &e.xxh3_full,
                    // No local full hash for this sampling → keep wanted (safe).
                    None => true,
                })
                .map(|e| e.rel_path.clone())
                .collect()
        }
    }

    fn oe(rel: &str, sampling: &str) -> OfferEntry {
        OfferEntry {
            rel_path: rel.into(),
            sampling_hash: sampling.into(),
            byte_size: 1,
        }
    }

    #[tokio::test]
    async fn negotiate_returns_only_absent_and_false_positive_wants() {
        let net = LoopbackNetwork::new();

        // The receiver "has" sampling "have" with full "F_HAVE".
        let responder = StubResponder {
            present: ["have".to_string()].into_iter().collect(),
            local_full: [("have".to_string(), "F_HAVE".to_string())]
                .into_iter()
                .collect(),
        };
        let recv = net.endpoint_with_responder(Arc::new(responder));
        recv.start().await.unwrap();
        let node_b = recv.node_id();

        let send = net.endpoint();
        send.start().await.unwrap();

        let offer = vec![
            oe("new.fits", "absent"),   // sampling absent → want
            oe("have.fits", "have"),    // candidate, full matches → drop
            oe("collide.fits", "have"), // candidate, full differs → false positive → want
        ];
        let full: HashMap<String, String> = [
            ("have.fits".to_string(), "F_HAVE".to_string()),
            ("collide.fits".to_string(), "F_OTHER".to_string()),
        ]
        .into_iter()
        .collect();

        let want = send
            .negotiate_want(node_b, PackageId("p".into()), offer, full)
            .await
            .unwrap();

        assert_eq!(
            want,
            ["new.fits".to_string(), "collide.fits".to_string()]
                .into_iter()
                .collect::<HashSet<String>>()
        );
    }

    #[tokio::test]
    async fn negotiate_without_responder_wants_everything() {
        // A responder-less receiver (send-only-less full peer) must still be
        // offered everything: no dedup, want-all.
        let net = LoopbackNetwork::new();
        let recv = net.endpoint();
        recv.start().await.unwrap();
        let node_b = recv.node_id();
        let send = net.endpoint();
        send.start().await.unwrap();

        let offer = vec![oe("a.fits", "h1"), oe("b.fits", "h2")];
        let want = send
            .negotiate_want(node_b, PackageId("p".into()), offer, HashMap::new())
            .await
            .unwrap();
        assert_eq!(
            want,
            ["a.fits".to_string(), "b.fits".to_string()]
                .into_iter()
                .collect::<HashSet<String>>()
        );
    }
}
