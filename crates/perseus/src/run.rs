//! The running agent: durable sync store, ONE shared iroh node with a
//! `Role::Out` handle + sync engine **per configured send target**, and the
//! capture watcher wired together (Sync 2C multi-target send).
//!
//! [`Agent`] is transport-injectable. Production ([`Agent::start`]) resolves every
//! target ([`crate::account::resolve_targets`]) and binds ONE
//! [`SharedIrohNode`](athenaeum_core::sharing::iroh::node::SharedIrohNode) from
//! this install's device key — a single endpoint + blob store — then gives every
//! resolved target its own [`Role::Out`](athenaeum_core::sharing::iroh::node::Role)
//! handle onto that node + one [`SyncEngine`] per target, all sharing the one
//! durable store. Binding one endpoint *per target* (the previous shape)
//! self-collided on the relay: a relay keeps only one connection per node id, so
//! a multi-destination batch evicted its own peers (the same C1 field incident
//! the app hit). The Task 2 event demux disambiguates each target's acks by
//! `(peer, package)`, so the engines stay one-per-peer over the shared node. The
//! enqueue pipeline builds each package ONCE and fans it out to every engine — a
//! per-target failure is `warn!`-logged and never drops the others (spec §8).
//! Tests ([`Agent::start_with_transport`] for a single target,
//! [`Agent::start_with_transports`] for N) inject in-process loopback transports
//! and known peers, exercising the exact same enqueue/package/engine path without
//! a network (and without binding a node).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use athenaeum_core::fits_parser::{parse_fits_with_header, parse_xisf};
use athenaeum_core::package::{
    self, read_manifest, write_package, ManifestRecord, PayloadKind, MANIFEST_VERSION,
};
use athenaeum_core::sharing::iroh::node::{Role, SharedIrohNode};
use athenaeum_core::sharing::iroh::random_secret;
use athenaeum_core::sharing::types::NodeId;
use athenaeum_core::sharing::SharingTransport;
use athenaeum_core::sync::engine::AddrRefresher;
use athenaeum_core::sync::store::{StandaloneSyncStore, SyncStore};
use athenaeum_core::sync::{
    evaluate_and_apply, node_id_hex, DeleteOutcome, Direction, HistoryRow, OutboundRow,
    OutboundState, PackageCleanupSink, RetentionOutcome, SharedPackageCleanup, SyncEngine,
    SyncEngineHandle,
};
use chrono::{DateTime, Utc};
use iroh_tickets::endpoint::EndpointTicket;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::batch_store::BatchStore;
use crate::batcher::{spawn_batcher, BatcherHandle};
use crate::config::{Config, RetentionConfig, SendCfg};
use crate::seen::{SeenStore, SourceLink};
use crate::watcher::{self, WatcherHandle};
use crate::web::RetentionRunRecord;

/// Parse a pairing ticket string into its `EndpointTicket`. Factored out so the
/// string is only ever parsed once per call site: [`Agent::start`] derives both
/// the peer [`NodeId`] and its dialable address from a single parse and hands
/// the address to the transport directly (`add_peer`, not the
/// re-parsing `add_peer_ticket`).
fn parse_ticket(ticket: &str) -> Result<EndpointTicket> {
    ticket
        .parse()
        .context("parse pairing_ticket as an iroh endpoint ticket")
}

/// Derive the peer's [`NodeId`] from a pairing ticket (an iroh `EndpointTicket`).
pub fn peer_node_id_from_ticket(ticket: &str) -> Result<NodeId> {
    let ticket = parse_ticket(ticket)?;
    Ok(*ticket.endpoint_addr().id.as_bytes())
}

/// Build the shared iroh node's relay-map resolver (iroh hardening T8, H2). The
/// node's hourly refresh loop re-runs this to learn the CURRENT relay map
/// ([`crate::account::resolve_relay_config`]); a changed set drives an idle
/// endpoint rebuild. Captures an owned `Config` so it can run in the detached
/// refresh task; a resolve error yields `None` (keep the current relay map).
fn build_relay_resolver(config: Config) -> athenaeum_core::sharing::iroh::node::RelayResolver {
    let config = Arc::new(config);
    Arc::new(move || {
        let config = Arc::clone(&config);
        Box::pin(async move {
            match crate::account::resolve_relay_config(&config).await {
                Ok(pair) => Some(pair),
                Err(e) => {
                    tracing::warn!(
                        error = %format!("{e:#}"),
                        "relay refresh: resolve failed; keeping current relay map"
                    );
                    None
                }
            }
        })
    })
}

/// Build the retry-time peer-address refresher (T8): on a timed-out send retry,
/// re-resolve the target's CURRENT address (a fresh [`crate::account::resolve_targets`]
/// → [`peer_dial_addr`](athenaeum_core::sync::pairing::peer_dial_addr)) so a
/// relay-map change or a target moving relays doesn't strand retries on a dead
/// cached path. Dispatches on the peer arg, so one refresher serves every
/// per-target engine. `None` (target gone / hub unreachable) leaves the existing
/// address in place.
fn build_target_addr_refresher(config: Config) -> AddrRefresher {
    let config = Arc::new(config);
    Arc::new(move |peer: NodeId| {
        let config = Arc::clone(&config);
        Box::pin(async move {
            // `None`: retry-time re-resolution never rewrites the config file — a
            // rename was already healed (file + cache) at `Agent::start`, and this
            // path holds only the spawn-time in-memory config, not its path. A
            // still-renamed target resolves quietly from the cache heal.
            match crate::account::resolve_targets(&config, None).await {
                Ok(resolved) => {
                    let target = resolved.targets.iter().find(|t| t.peer == peer)?;
                    match athenaeum_core::sync::pairing::peer_dial_addr(
                        peer,
                        target.endpoint_addr.as_ref(),
                        &resolved.relay_urls,
                        false, // same account
                    ) {
                        Ok(addr) => Some(addr),
                        Err(e) => {
                            tracing::warn!(error = %format!("{e:#}"), "retry addr refresh: address build failed");
                            None
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "retry addr refresh: resolve_targets failed");
                    None
                }
            }
        })
    })
}

/// Load the persisted 32-byte device secret, creating it (mode 0600) on first
/// run. On every load of an existing key, permissions are re-checked and
/// tightened back to 0600 if a group/world bit has crept in (backup restore,
/// a loose umask, manual copy) — the identity secret must never be readable
/// by anyone but the service user.
pub fn load_or_create_device_key(path: &Path) -> Result<[u8; 32]> {
    if path.exists() {
        tighten_permissions_if_needed(path)?;
        let bytes =
            std::fs::read(path).with_context(|| format!("read device key {}", path.display()))?;
        let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
            anyhow::anyhow!(
                "device key {} is {} bytes, expected 32 — delete it to regenerate",
                path.display(),
                bytes.len()
            )
        })?;
        Ok(arr)
    } else {
        let secret = random_secret();
        write_secret_0600(path, &secret)?;
        tracing::info!(path = %path.display(), "generated new device key");
        Ok(secret)
    }
}

/// Re-check an existing device key's permissions and tighten to 0600 if any
/// group/other bit is set. No-op on non-Unix (no POSIX mode bits to check).
#[cfg(unix)]
fn tighten_permissions_if_needed(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta =
        std::fs::metadata(path).with_context(|| format!("stat device key {}", path.display()))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("tighten device key permissions {}", path.display()))?;
        tracing::warn!(
            path = %path.display(),
            old_mode = format!("{mode:o}"),
            "device key permissions tightened"
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn tighten_permissions_if_needed(_path: &Path) -> Result<()> {
    // No POSIX permission bits on this platform; nothing to tighten.
    Ok(())
}

#[cfg(unix)]
fn write_secret_0600(path: &Path, secret: &[u8; 32]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create device key {}", path.display()))?;
    f.write_all(secret)
        .with_context(|| format!("write device key {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_secret_0600(path: &Path, secret: &[u8; 32]) -> Result<()> {
    std::fs::write(path, secret).with_context(|| format!("write device key {}", path.display()))?;
    Ok(())
}

/// Build a one-frame package for a single capture file and return its directory.
///
/// MVP granularity is **one package per file** (documented choice): capture
/// software emits one FITS/XISF per sub-exposure, the watcher stabilizes them
/// one at a time, and a per-file package keeps enqueue/confirm/retry accounting
/// trivially one-to-one with a frame. The manifest's `frame_meta` is the full
/// header-derived [`athenaeum_core::models::Frame`]; `frame_uuid` is minted here
/// (Perseus is headless — there is no catalog uuid to inherit).
///
/// `capture_dir` is the watched root `file_path` came from: the manifest
/// `rel_path` is computed **relative to it** ([`compute_rel_path`]) so the
/// receiver can replicate the capture directory tree, and — when more than one
/// capture dir is configured — prefixed with a sanitized per-dir label so files
/// from different roots don't collide.
pub fn build_package_for_file(
    config: &Config,
    capture_dir: &Path,
    file_path: &Path,
    origin_device: &str,
) -> Result<PathBuf> {
    // One file is just a one-element batch; delegate so the manifest-record shape
    // is defined in exactly one place (Task 3). A single missing/unparseable file
    // means the batch is empty → `build_batch_package` errors, matching the old
    // fail-fast contract `Agent::enqueue_file` relies on.
    let input = [(capture_dir.to_path_buf(), file_path.to_path_buf())];
    build_batch_package(config, &input, origin_device).map(|(pkg_dir, _)| pkg_dir)
}

/// Build ONE package containing a manifest record per `(capture_dir, file)` in
/// `files`. A file that vanished, won't parse, or can't be hashed is dropped
/// with a `warn!` and the batch continues — a single bad frame never fails the
/// whole set. Returns `(pkg_dir, included)` where `included` is the capture-file
/// paths whose records actually made it into the package, in order
/// (`included.len()` is the packaged record count); empty input OR every file
/// dropped is an error (a package with zero records is never written).
///
/// Returning the *included* paths — not just a count — is what lets the batcher
/// flush ([`crate::batcher`]) record as seen **only** the files it truly
/// packaged. A present-but-unbuildable file dropped above must NOT be marked
/// seen: the durable seen store is the dedup authority, so marking it would lose
/// the frame forever; leaving it unseen makes it retried on the next detection /
/// restart, matching the legacy per-file path (a failed build was never seen).
///
/// Each record is byte-identical in shape to the single-file path (minted
/// `frame_uuid`, `origin_catalog_uuid == frame_uuid`, capture-dir-relative
/// `rel_path`, full-file `xxh3`, header-derived `frame_meta`). The package dir
/// is keyed by a fresh uuid (not any one frame's uuid) since it now carries N
/// frames.
pub fn build_batch_package(
    config: &Config,
    files: &[(PathBuf /* capture_dir */, PathBuf /* file */)],
    origin_device: &str,
) -> Result<(PathBuf, Vec<PathBuf>)> {
    let mut records: Vec<(PathBuf, ManifestRecord)> = Vec::with_capacity(files.len());
    for (capture_dir, file_path) in files {
        match build_manifest_record(config, capture_dir, file_path, origin_device) {
            Ok(record) => records.push((file_path.clone(), record)),
            Err(error) => {
                // Never fatal: a vanished / unparseable / unhashable file is
                // dropped and the rest of the batch proceeds (spec §8 style).
                tracing::warn!(path = %file_path.display(), %error, "skipping file in batch");
            }
        }
    }

    if records.is_empty() {
        anyhow::bail!("batch has no buildable files");
    }

    // The capture-file paths that actually made it into the package, in order —
    // captured BEFORE `records` is moved into `write_package`. The caller records
    // exactly these as seen (see the fn docs).
    let included: Vec<PathBuf> = records.iter().map(|(path, _)| path.clone()).collect();
    let pkg_dir = config.packages_dir().join(Uuid::new_v4().to_string());
    write_package(&pkg_dir, records)
        .with_context(|| format!("write batch package {}", pkg_dir.display()))?;
    Ok((pkg_dir, included))
}

/// Build the single manifest record for one capture file — the per-file work
/// shared by [`build_package_for_file`] and [`build_batch_package`]. Any of the
/// three fallible steps (parse header, stat, hash) short-circuits with `?`, so a
/// caller batching many files can treat one `Err` as "drop this file".
fn build_manifest_record(
    config: &Config,
    capture_dir: &Path,
    file_path: &Path,
    origin_device: &str,
) -> Result<ManifestRecord> {
    let frame = parse_frame(file_path)?;
    let frame_uuid = Uuid::new_v4().to_string();
    let rel_path = compute_rel_path(config, capture_dir, file_path);
    let byte_size = std::fs::metadata(file_path)
        .with_context(|| format!("stat capture file {}", file_path.display()))?
        .len();
    let xxh3 = package::xxh3_full_file(file_path)?;

    Ok(ManifestRecord {
        v: MANIFEST_VERSION,
        frame_uuid: frame_uuid.clone(),
        // No producer catalog: anchor origin identity to the minted frame uuid.
        origin_catalog_uuid: frame_uuid,
        origin_device: origin_device.to_string(),
        payload_kind: PayloadKind::RawFrame,
        rel_path,
        byte_size,
        xxh3,
        frame_meta: serde_json::to_value(&frame).context("serialize frame_meta")?,
        analysis: None,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        project: None,
    })
}

/// The manifest `rel_path` for `file_path`: its path **relative to
/// `capture_dir`**, forward-slash separated (so it is `validate_rel_path`-clean
/// on every platform). When more than one capture dir is configured, a sanitized
/// label — the capture dir's basename, lowercased to `[a-z0-9._-]` — is prefixed
/// as the first segment so identically-named files from different roots do not
/// collide on the receiver.
///
/// Pure (no filesystem access) so the path math is unit-testable without writing
/// a package. If `file_path` is somehow not under `capture_dir` the basename is
/// used as a safe fallback (a `rel_path` is never allowed to escape the root).
pub fn compute_rel_path(config: &Config, capture_dir: &Path, file_path: &Path) -> String {
    let rel = file_path
        .strip_prefix(capture_dir)
        .ok()
        .map(to_slash)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            file_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });

    if config.capture_dirs_resolved().len() > 1 {
        let label = sanitize_label(capture_dir);
        if !label.is_empty() {
            return format!("{label}/{rel}");
        }
    }
    rel
}

/// Join a path's `Normal` components with `/` — forward-slash on every platform,
/// and any root / prefix / `..` component is dropped, guaranteeing the result is
/// [`athenaeum_core::package::validate_rel_path`]-clean.
fn to_slash(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// A capture dir's basename as a single safe path segment: lowercased, with any
/// character outside `[a-z0-9._-]` replaced by `-`. Empty if the dir has no
/// usable basename (e.g. the filesystem root).
fn sanitize_label(capture_dir: &Path) -> String {
    let base = capture_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    base.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The configured capture dir that owns `file_path`: the longest configured root
/// that is an ancestor of the file, else the file's parent directory (so a file
/// outside every configured root still yields a bare-filename `rel_path`). Used
/// by [`Agent::enqueue_file`] (the `enqueue-backlog` path), where the owning
/// root is not carried by a watcher.
fn owning_capture_dir(config: &Config, file_path: &Path) -> PathBuf {
    config
        .capture_dirs_resolved()
        .into_iter()
        .filter(|d| file_path.starts_with(d))
        .max_by_key(|d| d.components().count())
        .unwrap_or_else(|| {
            file_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default()
        })
}

/// Parse a capture file's header into a [`Frame`](athenaeum_core::models::Frame)
/// by extension. `file_id` is irrelevant headless, so pass 0.
fn parse_frame(path: &Path) -> Result<athenaeum_core::models::Frame> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "xisf" => parse_xisf(path, 0).with_context(|| format!("parse XISF {}", path.display())),
        _ => parse_fits_with_header(path, 0)
            .map(|(frame, _header)| frame)
            .with_context(|| format!("parse FITS {}", path.display())),
    }
}

/// One resolved send target's running engine: the peer it sends to plus the
/// [`SyncEngine`] bound to it. Perseus fans each built package out to every
/// [`TargetEngine`] (Sync 2C multi-target send), one engine per target — each
/// riding its own `Role::Out` handle on the ONE shared iroh node — all sharing
/// the one durable [`StandaloneSyncStore`].
struct TargetEngine {
    /// The peer this engine sends to. Retained for diagnostics + to derive the
    /// audit `peer_device` (the engine itself already owns it).
    peer: NodeId,
    engine: Arc<SyncEngineHandle>,
}

/// A running capture agent. Owns one sync engine **per configured target**, the
/// durable store, the stat-aware seen store, and (optionally) the capture watcher
/// + its enqueue consumer.
pub struct Agent {
    config: Config,
    store: Arc<StandaloneSyncStore>,
    seen: Arc<SeenStore>,
    /// One engine per resolved send target (Sync 2C). Always non-empty: target
    /// resolution errors before an agent is built if zero targets resolve, and
    /// the injection seams require at least one transport. The enqueue pipeline
    /// builds each package once and fans it out to every engine here.
    engines: Vec<TargetEngine>,
    /// Shared-payload cleanup coordinator, `Some` ONLY for a true fan-out (≥2
    /// targets). One built package dir is served by every engine, so no single
    /// engine may delete it on its own confirm — that would starve a still-offline
    /// target's retry to a manifest-only collection (silent data loss). The
    /// coordinator frees the dir exactly once, after every target that received
    /// it is terminal. A single-target agent leaves this `None` and keeps the
    /// engine's original in-line cleanup, byte-for-byte.
    cleanup: Option<Arc<SharedPackageCleanup>>,
    /// The ONE shared iroh node backing every target's `Role::Out` engine (iroh
    /// hardening Task 4). `Some` on the production path ([`start`](Self::start)),
    /// `None` on the injection path (tests supply loopback transports and never
    /// bind a node). Held so [`shutdown`](Self::shutdown) can tear the single
    /// endpoint + store down and release the device-key advisory lock, letting a
    /// supervisor stop→start cleanly re-acquire it.
    node: Option<Arc<SharedIrohNode>>,
    origin_device: String,
    /// The configured sync peer id (hex) — the same value transfer history rows
    /// carry. Threaded into retention/manual-delete audit rows so a deleted
    /// source shows the peer it was sent to, not this node's own id. Carried into
    /// [`WebState`](crate::web::WebState) so `POST /api/delete` stamps it too.
    peer_device: String,
    /// One watcher per configured capture directory (empty when `watch` is
    /// false). All watchers feed the single enqueue pipeline; graceful shutdown
    /// drops ALL of them before draining the consumer.
    watchers: Vec<WatcherHandle>,
    /// The batcher loop's task (Sync Phase 2): accumulates the watcher's stable
    /// files and flushes them as one package per batch. `Some` only on the
    /// `watch` path (there is no batcher in enqueue-backlog mode). Replaces the
    /// old per-file `spawn_enqueue_consumer`.
    batcher_task: Option<JoinHandle<()>>,
    /// The batcher's control handle: the shared pending set + the manual-flush
    /// signal. `Some` only on the `watch` path. Cloned into the web layer (Task 6)
    /// so the status page can show "N pending" and trigger "Send N pending".
    batcher: Option<BatcherHandle>,
    /// Live-edit channel for the send config (mode + auto quiet window). Seeded
    /// with the startup [`Config::send_cfg`]; the batcher holds the matching
    /// receiver and adopts a new value on the next select! turn (no restart). The
    /// web settings page (Task 6) obtains a clone via [`Agent::send_cfg_tx`] and
    /// pushes re-validated edits. Created unconditionally (like `retention_tx`) so
    /// the accessor is always available; on the non-`watch` path there is no
    /// receiver, so a send is a harmless no-op.
    send_cfg_tx: watch::Sender<SendCfg>,
    retention_task: Option<JoinHandle<()>>,
    /// Live-edit channel for the retention config (task 8). The retention loop
    /// holds the matching receiver and re-borrows it every pass, so a `send`
    /// here takes effect on the next tick without an agent restart. The web
    /// settings page (tasks 9/10) obtains a clone via [`Agent::retention_tx`]
    /// and sends the re-validated config returned by
    /// [`crate::config_edit::apply_retention_edit`]. Dropping this sender (agent
    /// shutdown) is the retention loop's second, graceful exit path.
    retention_tx: watch::Sender<RetentionConfig>,
    /// Rolling record (cap 50, newest-first) of the retention loop's recent
    /// passes. The loop push-fronts each pass here; the web status page (task 10)
    /// reads a clone into [`WebState`](crate::web::WebState) at
    /// [`spawn_web_server`](Self::spawn_web_server) and serves it read-only at
    /// `GET /api/retention/log`. Empty on the non-`watch` path (no retention loop).
    retention_log: Arc<Mutex<VecDeque<RetentionRunRecord>>>,
}

impl Agent {
    /// Start a production agent: ONE shared iroh node (bound from the persistent
    /// device key), and the peers + relays resolved via the shared resolver
    /// (task M1) — account pairing (primary from the hub device list) → dev ticket
    /// → error. `watch` arms the capture watcher (true for `run`, false for
    /// `enqueue-backlog`).
    ///
    /// `config_path` is the on-disk `perseus.toml` this config was loaded from. It
    /// is retained in the signature for the supervisor's production launcher
    /// ([`crate::supervisor::production_launcher`]) and passed to
    /// [`resolve_targets`](crate::account::resolve_targets) so a renamed send
    /// target self-heals its config entry to the device id. Web-status-page
    /// ownership has moved OFF the agent onto the supervisor, which attaches
    /// [`WebState`](crate::web::WebState) to the running engine through its
    /// `on_agent` seam (Task 4 restores that wiring on this branch). `start` no
    /// longer binds the status page itself.
    pub async fn start(config: Config, config_path: PathBuf, watch: bool) -> Result<Self> {
        std::fs::create_dir_all(&config.data_dir)
            .with_context(|| format!("create data dir {}", config.data_dir.display()))?;

        // Resolve EVERY send target + the shared relay map before binding the
        // node: account resolution when signed in (offline per-target cache
        // fallback), else the dev ticket (a single target). Pass the config path
        // so a renamed target rewrites its entry to the device id in place.
        let resolved = crate::account::resolve_targets(&config, Some(&config_path)).await?;

        // ONE shared iroh node per agent (iroh hardening Task 4). A single
        // endpoint + blob store bound from this install's device key at
        // `<data_dir>/device_key` (the exact key the hub registered at sign-in),
        // instead of one endpoint per target — binding N endpoints from the SAME
        // key made a multi-destination batch self-collide on the relay (only one
        // connection per node id survives). The T1 device-key advisory lock is
        // taken here, so a second Perseus on the same `data_dir` fails loudly.
        let node = SharedIrohNode::bind(&config.data_dir, resolved.relay_mode.clone())
            .await
            .context("bind shared iroh node")?;
        let node_id = node.node_id();

        // Report THIS agent's dialable endpoint address to the hub (finding H1,
        // T7): a fire-and-forget task that polls the node's address and PUTs it on
        // change. Only when signed in (a dev-ticket run has no hub); never blocks
        // the bind — spawned and detached, self-terminating on node drop.
        if let Some((hub_url, token)) = crate::account::hub_credentials(&config) {
            athenaeum_core::sync::pairing::spawn_endpoint_address_reporter(
                Arc::clone(&node),
                hub_url,
                token,
            );
        }

        // Bound relay-map staleness (iroh hardening T8, H2): start the node's
        // hourly relay-map refresh loop. Its resolver re-runs the relay half of
        // target resolution ([`crate::account::resolve_relay_config`]); a changed
        // relay set drives an idle endpoint rebuild (same node id, same store).
        node.start_relay_refresh(build_relay_resolver(config.clone()));

        // One `Role::Out` handle + engine per resolved target (engines stay
        // one-per-peer; the Task 2 demux disambiguates each target's acks by
        // `(peer, package)`). Register each peer at the NODE level:
        //   - ticket path — from the ticket's embedded relay + direct addresses;
        //   - account path — the peer is a bare node id, and the node's endpoint
        //     has NO discovery services (`presets::Minimal`), so `announce` would
        //     fail instantly without a dial hint. Attach our own resolved relay
        //     URL(s) — the shared set account devices on this hub publish — as the
        //     peer's dial hint.
        let mut transports: Vec<(NodeId, Arc<dyn SharingTransport>)> = Vec::new();
        for target in &resolved.targets {
            if let Some(ticket) = &target.ticket {
                node.add_peer_ticket(ticket)
                    .context("register peer address from pairing ticket")?;
            } else {
                // Account path: prefer the target's OWN hub-reported address (its
                // real home relay + direct addrs — same account, so direct is
                // allowed) via `peer_dial_addr`, falling back to the shared relay
                // set when the target never reported (T7 / finding H1).
                let peer_addr = athenaeum_core::sync::pairing::peer_dial_addr(
                    target.peer,
                    target.endpoint_addr.as_ref(),
                    &resolved.relay_urls,
                    false, // same account
                )
                .context("construct account-resolved peer address")?;
                node.add_peer(peer_addr);
            }
            tracing::info!(
                node_id = %node_id_hex(&node_id),
                peer = %node_id_hex(&target.peer),
                "iroh out handle ready"
            );
            transports.push((target.peer, node.handle(Role::Out)));
        }

        // T8 retry re-resolution: on a timed-out send retry, re-resolve the
        // target's CURRENT address (a fresh `resolve_targets` → `peer_dial_addr`)
        // so a relay-map change or a target moving relays doesn't strand retries.
        let refresher = build_target_addr_refresher(config.clone());
        let mut agent =
            Self::start_with_transports(config, transports, node_id, watch, Some(refresher)).await?;
        // Retain the node so `shutdown` can tear the single endpoint + store down
        // and release the device-key lock (a supervisor stop→start then cleanly
        // re-acquires it). The injection seams leave `node` as `None`.
        agent.node = Some(node);
        // The embedded web status page is no longer bound here: its ownership
        // moved to the supervisor (Task 4), which attaches `WebState` to the
        // running engine via its `on_agent` seam. `bind_and_spawn_web` stays
        // `pub(crate)` for that caller.
        Ok(agent)
    }

    /// Start an agent over a SINGLE caller-supplied transport + peer. This is the
    /// injection seam the e2e test uses to run against a loopback transport; the
    /// single-target case delegates to [`start_with_transports`].
    #[doc(hidden)]
    pub async fn start_with_transport(
        config: Config,
        transport: Arc<dyn SharingTransport>,
        peer: NodeId,
        node_id: NodeId,
        watch: bool,
    ) -> Result<Self> {
        Self::start_with_transports(config, vec![(peer, transport)], node_id, watch, None).await
    }

    /// Start an agent over N caller-supplied `(peer, transport)` pairs — one sync
    /// engine per target, all sharing the one durable store, and the enqueue
    /// pipeline fanning each built package out to all of them (Sync 2C
    /// multi-target send). The production path ([`start`](Self::start)) routes
    /// through here after building one iroh transport per resolved target; tests
    /// inject loopback transports.
    ///
    /// `transports` must be non-empty (there is always at least one target); the
    /// first pair's peer is used to derive the audit `peer_device`.
    #[doc(hidden)]
    pub async fn start_with_transports(
        config: Config,
        transports: Vec<(NodeId, Arc<dyn SharingTransport>)>,
        node_id: NodeId,
        watch: bool,
        addr_refresher: Option<AddrRefresher>,
    ) -> Result<Self> {
        std::fs::create_dir_all(&config.data_dir)
            .with_context(|| format!("create data dir {}", config.data_dir.display()))?;
        anyhow::ensure!(
            !transports.is_empty(),
            "an agent needs at least one send target"
        );

        let store = Arc::new(
            StandaloneSyncStore::open(config.db_path())
                .with_context(|| format!("open sync store {}", config.db_path().display()))?,
        );
        // Perseus's own store-aware dedup table, opened as a second connection
        // into the same `perseus.db` file (safe under WAL — see `crate::seen`).
        let seen = Arc::new(
            SeenStore::open(config.db_path())
                .with_context(|| format!("open seen store {}", config.db_path().display()))?,
        );
        // Shared-payload cleanup coordinator — ONLY for a true fan-out (≥2
        // targets). With one target there is no shared dir, so the engine's
        // original in-line cleanup is correct and left byte-for-byte unchanged
        // (`cleanup = None`, plain `SyncEngine::spawn`). With ≥2 targets every
        // engine is given the SAME coordinator so the one built package dir is
        // freed exactly once, after every target is terminal.
        //
        // Reconcile FIRST, before any engine spawns: in-memory coordinator state
        // is lost on restart, so re-derive it from the durable rows (grouped by
        // package dir) — a dir terminal on all targets is cleaned now, one still
        // pending keeps its payload. Running before spawn means no engine's
        // resume can confirm/clean ahead of the seeded expected counts.
        let cleanup: Option<Arc<SharedPackageCleanup>> = if transports.len() > 1 {
            let coord = Arc::new(SharedPackageCleanup::new());
            if let Err(error) = reconcile_shared_cleanup(&store, &coord) {
                tracing::warn!(%error, "shared-cleanup restart reconciliation failed; continuing");
            }
            Some(coord)
        } else {
            None
        };
        // One engine per target, all over the single shared store. The engine's
        // crash-resume is peer-scoped (core `sync::engine`), so N engines on one
        // store re-drive only their own outbound rows.
        let engines: Vec<TargetEngine> = transports
            .into_iter()
            .map(|(peer, transport)| {
                // Each per-target engine shares the one refresher (it dispatches on
                // the peer arg), so a timed-out retry re-resolves that target's
                // current address (T8). `None` on the injection/test seams.
                let engine = match &cleanup {
                    Some(coord) => SyncEngine::spawn_with_sink_and_refresher(
                        Arc::clone(&store) as Arc<dyn SyncStore>,
                        transport,
                        peer,
                        Arc::clone(coord) as Arc<dyn PackageCleanupSink>,
                        addr_refresher.clone(),
                    ),
                    None => SyncEngine::spawn_with_refresher(
                        Arc::clone(&store) as Arc<dyn SyncStore>,
                        transport,
                        peer,
                        addr_refresher.clone(),
                    ),
                };
                TargetEngine {
                    peer,
                    engine: Arc::new(engine),
                }
            })
            .collect();
        let origin_device = node_id_hex(&node_id);
        // The audit `peer_device` (hex) stamped on retention / manual-delete
        // history rows. With multiple targets a confirmed package was sent to
        // several peers; the FIRST target's id is used as the representative peer
        // for the audit column (the confirmed-gate itself is per-outbound-row).
        let peer_device = node_id_hex(&engines[0].peer);
        // Handles cloned out for the watcher/enqueue consumer fan-out below.
        let engine_handles: Vec<Arc<SyncEngineHandle>> =
            engines.iter().map(|t| Arc::clone(&t.engine)).collect();

        // Retention is ACTIVE (task A8). Live deletion is GATED behind the
        // two-key soak opt-in (task M4): `dry_run = false` is only accepted when
        // `i_have_verified_the_soak = true` (config validation enforces this), so
        // no files are deleted unless the owner has explicitly gone live. The
        // startup banner records policy + dry-run + opt-in state so the running
        // mode is unambiguous in the logs.
        tracing::info!(
            retention_policy = ?config.retention.policy,
            dry_run = config.retention.dry_run,
            soak_opt_in = config.retention.i_have_verified_the_soak,
            live_deletion = !config.retention.dry_run,
            interval_secs = config.retention.interval_secs,
            keep_days = config.retention.keep_days,
            disk_max_pct = config.retention.disk_max_pct,
            "retention active"
        );

        // Live-edit channel for the retention config (task 8). Seeded with the
        // startup config; the retention loop re-borrows it every pass. Created
        // unconditionally so `retention_tx()` is always available — when `watch`
        // is false there is no receiver (retention doesn't run in
        // enqueue-backlog), so a send is a harmless no-op.
        let (retention_tx, retention_rx) = watch::channel(config.retention.clone());

        // Rolling record of retention passes for the web status page (task 10).
        // Created unconditionally so the field is always present; only the
        // retention loop (below, `watch` path) ever writes to it.
        let retention_log: Arc<Mutex<VecDeque<RetentionRunRecord>>> =
            Arc::new(Mutex::new(VecDeque::new()));

        // Live-edit channel for the send config (Sync Phase 2). Seeded with the
        // startup mode + quiet window; the batcher re-borrows it on every select!
        // turn. Created unconditionally so `send_cfg_tx()` is always available —
        // when `watch` is false there is no batcher receiving it (a send is a
        // harmless no-op), matching the retention_tx pattern above.
        let (send_cfg_tx, send_cfg_rx) = watch::channel(config.send_cfg());

        let (watchers, batcher_task, batcher, retention_task) = if watch {
            // Each stable file is paired with the (canonicalized) capture dir it
            // came from, so the consumer can compute a capture-dir-relative
            // rel_path (with a per-dir label when watching more than one root).
            let (stable_tx, stable_rx) = mpsc::channel::<(PathBuf, PathBuf)>(64);
            // One watcher per configured capture directory; each gets its own
            // clone of the shared stable-file sender, all feeding the single
            // enqueue consumer below.
            let mut watchers = Vec::new();
            for dir in config.capture_dirs_resolved() {
                watchers.push(watcher::spawn_watcher(
                    dir,
                    config.stability(),
                    config.poll_interval(),
                    stable_tx.clone(),
                    Arc::clone(&seen),
                )?);
            }
            // Drop our own sender: the batcher's channel now closes precisely
            // when the LAST watcher drops its clone (i.e. all have shut down).
            drop(stable_tx);
            // Per-batch send bookkeeping (Task 2), opened as another connection
            // into the same `perseus.db` (WAL-safe, like `seen`). The batcher
            // writes one row per flushed package; the web history page (Task 6)
            // lists them.
            let batches = Arc::new(
                BatchStore::open(config.db_path())
                    .with_context(|| format!("open batch store {}", config.db_path().display()))?,
            );
            // Sync Phase 2: the batcher replaces the per-file consumer —
            // accumulate stable files, flush the whole set as ONE package on the
            // auto quiet-timer or a manual signal, fan it to every target.
            let (batcher, batcher_task) = spawn_batcher(
                stable_rx,
                engine_handles.clone(),
                Arc::clone(&seen),
                batches,
                config.clone(),
                origin_device.clone(),
                cleanup.clone(),
                send_cfg_rx,
            );
            let retention_task = spawn_retention_task(
                Arc::clone(&store),
                Arc::clone(&seen),
                config.clone(),
                retention_rx,
                Arc::clone(&retention_log),
                peer_device.clone(),
            );
            (
                watchers,
                Some(batcher_task),
                Some(batcher),
                Some(retention_task),
            )
        } else {
            // No batcher / retention loop in enqueue-backlog mode: drop the
            // receivers so those channels have none (the senders are still held
            // for API symmetry).
            drop(retention_rx);
            drop(send_cfg_rx);
            (Vec::new(), None, None, None)
        };

        Ok(Self {
            config,
            store,
            seen,
            engines,
            cleanup,
            // Injection path: no shared node (loopback transports). `start` sets
            // this to the bound node on the production path.
            node: None,
            origin_device,
            peer_device,
            watchers,
            batcher_task,
            batcher,
            send_cfg_tx,
            retention_task,
            retention_tx,
            retention_log,
        })
    }

    /// Build a package for `file_path` **once** and enqueue it to EVERY target
    /// engine (Sync 2C multi-target send). Used by `enqueue-backlog` and reused by
    /// the watcher consumer. Records the file's current `(size, mtime)` in the
    /// seen store once at least one durable `Queued` row exists, so a later
    /// restart never re-packages this exact, unchanged file.
    ///
    /// Per spec §8 the targets are independent: a per-target enqueue failure is
    /// `warn!`-logged and never aborts the others. Errors only if the package
    /// could not be built at all, or if it reached ZERO targets.
    pub async fn enqueue_file(&self, file_path: &Path) -> Result<i64> {
        let capture_dir = owning_capture_dir(&self.config, file_path);
        let pkg_dir =
            build_package_for_file(&self.config, &capture_dir, file_path, &self.origin_device)?;
        let engines = self.engine_handles();
        let (first_id, delivered) = enqueue_package_to_all(&engines, &pkg_dir).await;
        // Fan-out only: tell the coordinator how many targets actually received
        // this dir, so it frees the shared payload exactly once — after every one
        // of them is terminal. `delivered == 0` (reached no target) registers an
        // expected of 0, which cleans the orphaned copy immediately (no target's
        // retry can ever need it). Single-target agents skip this entirely
        // (`cleanup` is `None`) and keep the engine's in-line cleanup.
        if let Some(coord) = &self.cleanup {
            coord.register(&pkg_dir, delivered);
        }
        let Some(first_id) = first_id else {
            anyhow::bail!(
                "package for {} reached none of the {} configured targets",
                file_path.display(),
                engines.len()
            );
        };
        tracing::info!(
            id = first_id,
            delivered,
            targets = engines.len(),
            path = %file_path.display(),
            "enqueued capture file"
        );
        record_seen(&self.seen, file_path, &pkg_dir.to_string_lossy());
        Ok(first_id)
    }

    /// Cheap clones of every target engine handle (fan-out call sites).
    fn engine_handles(&self) -> Vec<Arc<SyncEngineHandle>> {
        self.engines.iter().map(|t| Arc::clone(&t.engine)).collect()
    }

    /// The number of configured send targets (one engine each). Test/introspection.
    pub fn engine_count(&self) -> usize {
        self.engines.len()
    }

    /// Live in-flight (non-terminal) outbound rows across ALL targets. Every
    /// engine shares the one store, so any engine's snapshot is the same
    /// node-wide picture — read the first engine's.
    pub fn status_snapshot(&self) -> Result<Vec<OutboundRow>> {
        self.engines[0].engine.status_snapshot()
    }

    /// The durable store (test/introspection).
    pub fn store(&self) -> &Arc<StandaloneSyncStore> {
        &self.store
    }

    /// This agent's own device id (hex).
    pub fn origin_device(&self) -> &str {
        &self.origin_device
    }

    /// A cheap clone of the FIRST target's sync engine handle. The supervisor's
    /// [`ManagedAgent`](crate::supervisor::ManagedAgent) impl hands this to Task
    /// 4's web state so the status page reads live engine status (`status_snapshot`
    /// reads the shared store, so any engine gives the node-wide picture) and the
    /// web `retry` re-enqueues through it. Multi-target web-retry fan-out is not
    /// wired in v1 — a retried package goes to the first target only.
    pub fn engine_handle(&self) -> Arc<SyncEngineHandle> {
        Arc::clone(&self.engines[0].engine)
    }

    /// The configured sync peer id (hex) — the same value delete/audit history
    /// rows carry. Cloned into the supervisor's [`ManagedAgent`] view.
    pub fn peer_device(&self) -> String {
        self.peer_device.clone()
    }

    /// The shared-payload cleanup coordinator, `Some` only for a true fan-out
    /// (≥2 targets). Cloned into the supervisor's [`ManagedAgent`] view so the
    /// web retry (`POST /api/retry`) can [`bump`](SharedPackageCleanup::bump) it
    /// when it re-enqueues a failed package: the retry adds a new outbound row to
    /// the sinked engine, and without the bump that row's terminal would
    /// over-count against the coordinator's stale `expected` and free the shared
    /// payload while a still-offline target has yet to receive it. `None` for a
    /// single-target agent (no shared dir, no coordinator).
    pub fn cleanup(&self) -> Option<Arc<SharedPackageCleanup>> {
        self.cleanup.clone()
    }

    /// A shared handle to the rolling retention-pass log the web status page
    /// serves read-only. Cloned into the supervisor's [`ManagedAgent`] view.
    pub fn retention_log(&self) -> Arc<Mutex<VecDeque<RetentionRunRecord>>> {
        Arc::clone(&self.retention_log)
    }

    /// A clone of the retention live-edit sender (task 8). The web settings page
    /// sends the re-validated [`RetentionConfig`] returned by
    /// [`crate::config_edit::apply_retention_edit`] here to have the running
    /// retention loop adopt it on its next pass — no agent restart. When the
    /// agent was started without `watch` no receiver exists (a non-watch agent
    /// has no retention loop), so `send()` returns `Err(SendError)`; callers
    /// discard it.
    pub fn retention_tx(&self) -> watch::Sender<RetentionConfig> {
        self.retention_tx.clone()
    }

    /// The batcher's control handle (Sync Phase 2), `Some` only on the `watch`
    /// path (enqueue-backlog has no batcher). The web layer (Task 6) clones this
    /// to render "N pending" ([`BatcherHandle::pending_snapshot`]) and to trigger
    /// "Send N pending" ([`BatcherHandle::flush_now`]).
    pub fn batcher(&self) -> Option<BatcherHandle> {
        self.batcher.clone()
    }

    /// A clone of the send-config live-edit sender. The web settings page (Task 6)
    /// pushes a re-validated [`SendCfg`] (mode + auto quiet window) here for the
    /// running batcher to adopt on its next select! turn — no agent restart. When
    /// the agent was started without `watch` there is no batcher receiving it, so
    /// a `send` is a harmless no-op.
    pub fn send_cfg_tx(&self) -> watch::Sender<SendCfg> {
        self.send_cfg_tx.clone()
    }

    /// Gracefully stop the watcher, drain the batcher, and shut the engine down
    /// (awaiting its worker).
    pub async fn shutdown(self) {
        // The retention loop is an independent task holding only Arc clones;
        // abort it directly — there is nothing to drain.
        if let Some(t) = self.retention_task {
            t.abort();
        }
        // Shut down EVERY watcher before draining: each holds a clone of the
        // stable-file sender, so the consumer's channel only closes once the
        // last of them is gone.
        for w in self.watchers {
            w.shutdown().await;
        }
        // With all watchers dropped, the batcher sees its channel close and exits
        // (any still-pending files are re-detected on the next run via the seen
        // store, so nothing is lost by not force-flushing here).
        if let Some(t) = self.batcher_task {
            let _ = t.await;
        }
        // Shut every target engine down (awaiting each worker) FIRST: each engine
        // holds a `Role::Out` handle onto the shared node, whose endpoint must
        // outlive them.
        for t in self.engines {
            t.engine.shutdown().await;
        }
        // Then tear the ONE shared node down, bounded (Task 4): router + store
        // shutdown + a graceful endpoint close, releasing the device-key advisory
        // lock so a supervisor stop→start can re-acquire it. `None` on the
        // injection path. The node's own close is internally bounded; the outer
        // 5s guard is a belt-and-braces cap so shutdown can never hang exit.
        if let Some(node) = self.node {
            if tokio::time::timeout(std::time::Duration::from_secs(5), node.shutdown())
                .await
                .is_err()
            {
                tracing::warn!("shared iroh node shutdown timed out");
            }
        }
    }

    /// Hard-kill: abort the watcher and batcher tasks immediately (no
    /// graceful handshake), then drop this agent's engine handle. Once the
    /// aborted batcher task's own handle clone is also gone, the engine's
    /// command channel closes and its worker notices and exits on its own —
    /// the same mechanism [`shutdown`](Self::shutdown) relies on, just without
    /// the cooperative command-and-await round trip.
    ///
    /// Test-only: simulates a killed process (SIGKILL, power loss) for the
    /// crash-resume e2e test, where a plain `drop(agent)` would merely detach
    /// the background tasks — they keep running, which would let the "old"
    /// agent quietly finish the transfer itself and make the test pass for the
    /// wrong reason instead of proving the *new* agent's crash-resume works.
    #[doc(hidden)]
    pub fn kill_for_test(self) {
        for w in self.watchers {
            w.abort_for_test();
        }
        if let Some(t) = self.batcher_task {
            t.abort();
        }
        if let Some(t) = self.retention_task {
            t.abort();
        }
        // `self.engines` (and `self.store`/`self.seen`/`self.node`) drop here, at
        // end of scope: each dropped engine handle closes its worker's command
        // channel, so every worker notices and exits on its own (the same
        // mechanism `shutdown` relies on, minus the cooperative await). The shared
        // node's `Arc` drops WITHOUT a graceful `shutdown` — that is the point:
        // a killed process never closes its endpoint cleanly.
    }
}

/// Bind `web_bind` and spawn the axum status-page server, returning its task
/// handle — or `None` if the bind fails.
///
/// **Runtime bind failure is non-fatal.** The status page is an optional
/// convenience; the capture node's primary function is watching + syncing. A
/// port conflict on the default `127.0.0.1:8686` (a second agent, a stale
/// process, an unrelated service) must not take the whole agent down, so a bind
/// error is logged at `error!` and swallowed here — the agent runs on without a
/// web page. This is deliberately distinct from the `Config::validate` security
/// gate, which still *hard-refuses* startup for a non-loopback `web_bind`
/// without a `web_token`: that is a misconfiguration to fix, not a transient
/// runtime condition to ride through.
///
/// Called by [`supervisor::start_supervised`](crate::supervisor::start_supervised),
/// which binds the always-on status page once at startup (the web page's
/// ownership moved off the agent onto the supervisor in Task 4), and by the
/// `#[cfg(all(test, unix))]` tests below.
pub(crate) async fn bind_and_spawn_web(
    web_bind: &str,
    router: axum::Router,
) -> Option<JoinHandle<()>> {
    // Wrap the router with the DNS-rebinding Host guard (finding M2), derived
    // from the bind address. `web_bind` is already validated as a SocketAddr by
    // `Config::validate`; if it somehow does not parse, serve without the guard
    // rather than dropping the page (a loopback bind is the default anyway).
    let router = match web_bind.parse::<std::net::SocketAddr>() {
        Ok(addr) => crate::web::apply_host_guard(router, crate::web::HostPolicy::for_bind(addr)),
        Err(_) => router,
    };
    match tokio::net::TcpListener::bind(web_bind).await {
        Ok(listener) => {
            tracing::info!(bind = %web_bind, "web status page online");
            Some(tokio::spawn(async move {
                if let Err(e) = axum::serve(listener, router).await {
                    tracing::error!(error = %e, "web status page server exited");
                }
            }))
        }
        Err(e) => {
            tracing::error!(
                bind = %web_bind,
                error = %e,
                "web status page failed to bind; continuing without it"
            );
            None
        }
    }
}

/// Record `path`'s current `(size, mtime)` and its `package_ref` in the seen
/// store. Best-effort: a failure here only means a possible harmless re-send on a
/// future restart — it must never fail the enqueue itself (the file is already
/// durably queued). The `package_ref` linkage is what retention later joins on to
/// map a confirmed package back to this source capture file.
///
/// `pub(crate)` so the [`crate::batcher`] flush path records each file in a
/// flushed batch, exactly as the old per-file consumer did.
pub(crate) fn record_seen(seen: &SeenStore, path: &Path, package_ref: &str) {
    match std::fs::metadata(path) {
        Ok(m) => {
            let mtime_ms = crate::seen::mtime_millis(m.modified().ok());
            if let Err(error) = seen.mark_enqueued(path, m.len(), mtime_ms, package_ref) {
                tracing::warn!(%error, path = %path.display(), "failed to record seen-store entry");
            }
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "failed to stat file for seen-store entry");
        }
    }
}

/// Build one package and enqueue it to EVERY target engine (Sync 2C multi-target
/// send). The package is built once by the caller; this fans that one package dir
/// out to all engines. Per spec §8 the targets are independent — a per-engine
/// `enqueue_package` failure (e.g. a stopped worker) is `warn!`-logged and never
/// stops the others. Returns `(first_id, delivered_count)`: the durable row id of
/// the first successful enqueue (for the seen-store linkage / caller reporting)
/// and how many targets accepted it. `first_id` is `None` iff ZERO targets did.
///
/// `pub(crate)` so the [`crate::batcher`] flush path fans one batch package out
/// to every target, sharing the exact per-target failure isolation the per-file
/// path uses.
pub(crate) async fn enqueue_package_to_all(
    engines: &[Arc<SyncEngineHandle>],
    pkg_dir: &Path,
) -> (Option<i64>, usize) {
    let mut first_id: Option<i64> = None;
    let mut delivered = 0usize;
    for (idx, engine) in engines.iter().enumerate() {
        match engine.enqueue_package(pkg_dir).await {
            Ok(id) => {
                first_id.get_or_insert(id);
                delivered += 1;
            }
            Err(error) => {
                // One target failing must not drop the rest.
                tracing::warn!(
                    target_index = idx,
                    %error,
                    package = %pkg_dir.display(),
                    "enqueue to target failed; other targets are unaffected"
                );
            }
        }
    }
    (first_id, delivered)
}

/// Restart reconciliation for the shared-payload cleanup coordinator (fan-out
/// only). In-memory coordinator state is lost on restart, so re-derive it from
/// the durable `sync_outbound` rows BEFORE any engine spawns.
///
/// Each fanned-out package dir has one row per target it reached. Group by
/// `package_ref` and, per dir:
///   - `expected` = number of rows (the targets that received it), and
///   - already-terminal = rows in any terminal state (`Confirmed`, `Failed`, or
///     `Cancelled` — a cancel is stored as `Cancelled`), replayed as
///     `on_terminal` calls.
///
/// A dir terminal on every target is cleaned exactly once here; a dir with any
/// still-pending target keeps its payload — that target's engine resume re-drives
/// it, and its later confirm/fail notifies the coordinator to complete the gate.
///
/// Honest resume scope: this recovers precisely the all-terminal-vs-pending
/// decision, which is what both prevents the silent-loss bug (a premature delete
/// under a still-pending target) and bounds the payload leak. It does not
/// distinguish confirmed from failed targets — irrelevant to the cleanup gate,
/// where both are terminal.
fn reconcile_shared_cleanup(
    store: &StandaloneSyncStore,
    cleanup: &SharedPackageCleanup,
) -> Result<()> {
    use std::collections::HashMap;
    // Every row regardless of state; `u32::MAX` so a long-lived node's history is
    // never truncated (confirmed rows accumulate — retention deletes sources, not
    // rows).
    let rows = store.all_outbound(u32::MAX)?;
    let mut by_dir: HashMap<String, (usize, usize)> = HashMap::new();
    for row in &rows {
        let entry = by_dir.entry(row.package_ref.clone()).or_insert((0, 0));
        entry.0 += 1;
        if row.state.is_terminal() {
            entry.1 += 1;
        }
    }
    for (dir, (expected, terminal)) in by_dir {
        let path = PathBuf::from(&dir);
        cleanup.register(&path, expected);
        for _ in 0..terminal {
            cleanup.on_terminal(&path);
        }
    }
    Ok(())
}

/// RFC3339 UTC millisecond timestamp — the sync tables' canonical rendering.
fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Current disk usage of the volume holding `path`, as a whole percent
/// (`0..=100`). Only the `disk_pct` retention policy consults it.
///
/// Fails **safe**: any error, or a platform without `statvfs`, returns `0`
/// ("empty disk") so retention's disk-pressure gate can never *trigger* a
/// deletion off a bad reading — it can only ever decline to delete.
#[cfg(unix)]
fn disk_usage_pct(path: &Path) -> u8 {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let Ok(cpath) = CString::new(path.as_os_str().as_bytes()) else {
        return 0;
    };
    // SAFETY: `stat` is zero-initialised and only read after a successful call;
    // `cpath` is a valid NUL-terminated C string living for the call's duration.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(cpath.as_ptr(), &mut stat) };
    if rc != 0 {
        tracing::warn!(path = %path.display(), "statvfs failed; disk probe returns 0%");
        return 0;
    }
    let total = stat.f_blocks as u128;
    let avail = stat.f_bavail as u128;
    if total == 0 {
        return 0;
    }
    let used = total.saturating_sub(avail);
    ((used * 100) / total).min(100) as u8
}

#[cfg(not(unix))]
fn disk_usage_pct(_path: &Path) -> u8 {
    // No statvfs; treat as empty so retention never deletes on disk pressure.
    0
}

/// Pure guard: does `current_size`/`current_mtime_ms` match the `(size,
/// mtime_ms)` perseus recorded for this source at enqueue time? Extracted as a
/// free function purely so the TOCTOU-guard test can drive the comparison
/// directly rather than depending on real filesystem race timing.
fn source_stat_unchanged(link: &SourceLink, current_size: u64, current_mtime_ms: i64) -> bool {
    link.size == current_size && link.mtime_ms == current_mtime_ms
}

/// Build the `sync_history` audit row(s) for deleting `source`. The package
/// manifest is the source of truth for frame identity/bytes/object; when it
/// can't be read (or is unexpectedly empty) this falls back to one minimal row
/// (blank identity fields, `bytes` from the live file stat) — a degraded audit
/// beats no audit at all, but there is always at least one row to persist.
///
/// `outcome` is the audit tag stamped on every row — `retention_deleted` for an
/// automatic retention pass, `deleted_manual` for the web "Delete selected"
/// action ([`delete_confirmed_packages`]). Both take the exact same deletion
/// path; only this tag distinguishes them in the history log.
///
/// `peer_device` is the CONFIGURED SYNC PEER id (hex) — the same value transfer
/// history rows carry ([`SyncEngine`]'s sender stamps `node_id_hex(&peer)`).
/// Stamping it here (rather than the manifest's `origin_device`, which is *this*
/// node's own id) makes a deleted-source audit row show the peer the confirmed
/// package was sent to, consistent with its matching transfer row.
fn build_retention_history_rows(
    pkg_ref: &Path,
    source: &Path,
    byte_size: u64,
    outcome: &str,
    peer_device: &str,
) -> Vec<HistoryRow> {
    let records = match read_manifest(pkg_ref) {
        Ok(records) => records,
        Err(error) => {
            tracing::warn!(
                package_ref = %pkg_ref.display(),
                %error,
                "retention: manifest unreadable; writing a minimal fallback audit row"
            );
            Vec::new()
        }
    };

    if records.is_empty() {
        let filename = source
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        return vec![HistoryRow {
            frame_uuid: String::new(),
            filename,
            object: None,
            peer_device: peer_device.to_string(),
            direction: Direction::Sent,
            bytes: byte_size,
            started_at: now_iso(),
            finished_at: Some(now_iso()),
            outcome: outcome.to_string(),
            // Perseus is a personal-sync capture agent — no project dimension.
            project: None,
            // No per-batch detail surface on the Perseus agent (Task 14).
            package_id: None,
        }];
    }

    records
        .iter()
        .map(|r| {
            let object = r
                .frame_meta
                .get("object")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            HistoryRow {
                frame_uuid: r.frame_uuid.clone(),
                filename: r.rel_path.clone(),
                object,
                // The sync peer (whom the package was sent to), NOT the
                // manifest's `origin_device` (this node itself) — see fn docs.
                peer_device: peer_device.to_string(),
                direction: Direction::Sent,
                bytes: r.byte_size,
                started_at: now_iso(),
                finished_at: Some(now_iso()),
                outcome: outcome.to_string(),
                // Perseus is a personal-sync capture agent — no project dimension.
                project: None,
                // No per-batch detail surface on the Perseus agent (Task 14).
                package_id: None,
            }
        })
        .collect()
}

/// The retention deleter (fs remove + audit history + seen-store update) for one
/// confirmed package. `pkg_ref` is `sync_outbound.package_ref` (the package
/// directory) — the abstract deletable subject core hands us; here we resolve it
/// back to the *original source capture file* and remove that.
///
/// Implements the deleter contract from `sync::retention`'s module docs:
///
/// - **Audit before delete.** The `sync_history` row(s) are persisted BEFORE
///   `remove_file` runs; if persistence fails, the `?` below propagates and the
///   file is never touched — a delete only ever happens once it is durably
///   discoverable that it happened.
/// - **Honest [`DeleteOutcome`].** Only a real `remove_file` reports `Removed`;
///   every legitimate no-op (no live linkage, already gone out-of-band, stat
///   drift since confirmation) reports `SkippedNoop` and is never mistaken for
///   an actual deletion by the caller.
/// - **Last-line TOCTOU guard.** Immediately before removal, the source is
///   re-stat'd and compared against what was recorded when it was enqueued — a
///   concurrent re-enqueue rewriting this exact path since confirmation aborts
///   the delete rather than destroying new, unconfirmed content.
///
/// `store` is `&dyn SyncStore` (not the concrete `StandaloneSyncStore`) purely
/// for testability: it lets a test inject a store whose `append_history`
/// deliberately fails, proving the audit-before-delete refusal.
///
/// `outcome` is the audit tag (`retention_deleted` for a retention pass,
/// `deleted_manual` for a web delete) — the deletion path is byte-for-byte
/// identical either way, so the two share this one function and the same
/// safety contract. `peer_device` is the configured sync peer id (hex) stamped
/// onto the audit row(s) (see [`build_retention_history_rows`]).
fn retention_delete_source(
    store: &dyn SyncStore,
    seen: &SeenStore,
    pkg_ref: &Path,
    outcome: &str,
    peer_device: &str,
) -> Result<DeleteOutcome> {
    let pkg_ref_str = pkg_ref.to_string_lossy();

    let Some(link) = seen.source_for_package(&pkg_ref_str)? else {
        tracing::debug!(
            package_ref = %pkg_ref.display(),
            "retention: no live source linkage for confirmed package; skipping (already deleted or superseded)"
        );
        return Ok(DeleteOutcome::SkippedNoop);
    };
    let source = link.path.clone();

    // Out-of-band removal: something other than retention already removed the
    // file (manual cleanup, external tool). Stamp the linkage dead so it stops
    // being offered again, but this was never a retention deletion — no audit
    // row, and a distinct, honest outcome tag in the log.
    if !source.exists() {
        tracing::info!(
            path = %source.display(),
            package_ref = %pkg_ref.display(),
            outcome = "retention_source_missing",
            "retention: source already gone out-of-band; marking linkage dead"
        );
        seen.mark_deleted(&source)?;
        return Ok(DeleteOutcome::SkippedNoop);
    }

    // TOCTOU last-line guard: re-stat immediately before removal and require an
    // exact match against what was recorded when this source was enqueued. A
    // mismatch means a concurrent re-enqueue rewrote this path with new
    // (unconfirmed) content since the package was confirmed — deleting it would
    // destroy live, never-synced data.
    let current_meta = std::fs::metadata(&source)
        .with_context(|| format!("stat source before delete {}", source.display()))?;
    let current_mtime_ms = crate::seen::mtime_millis(current_meta.modified().ok());
    if !source_stat_unchanged(&link, current_meta.len(), current_mtime_ms) {
        tracing::warn!(
            path = %source.display(),
            package_ref = %pkg_ref.display(),
            "retention skip: source changed since confirmation"
        );
        return Ok(DeleteOutcome::SkippedNoop);
    }

    // ── Audit BEFORE the destructive action ─────────────────────────────────
    // If even the fallback row can't be persisted, the `?` propagates and the
    // delete is refused entirely this tick — `source` is never touched.
    let history_rows =
        build_retention_history_rows(pkg_ref, &source, current_meta.len(), outcome, peer_device);
    for h in &history_rows {
        store
            .append_history(h.clone())
            .with_context(|| format!("persist retention audit for {}", source.display()))?;
    }

    // Only now, with the audit durably persisted, perform the destructive
    // action.
    std::fs::remove_file(&source)
        .with_context(|| format!("retention delete source {}", source.display()))?;

    // Best-effort: a failure here is logged but never rolled back — the audit
    // row already exists, so "this was deleted and audited" is already the
    // durable fact; only the seen-store's own bookkeeping would be stale.
    if let Err(error) = seen.mark_deleted(&source) {
        tracing::error!(
            path = %source.display(),
            %error,
            "retention: failed to stamp seen row deleted after a successful delete"
        );
    }

    Ok(DeleteOutcome::Removed)
}

/// Run one retention pass synchronously: map the config policy onto core's
/// evaluator and apply it with the fs-deleter. Split out from the tick loop so
/// it is directly unit-testable (inject `now` + a `disk_probe`).
///
/// The `store` reference serves double duty — it is both core's candidate source
/// (`&dyn SyncStore`) and the deleter's history sink; both are shared borrows, so
/// this composes without contention.
pub fn run_retention_once(
    config: &Config,
    store: &StandaloneSyncStore,
    seen: &SeenStore,
    now: DateTime<Utc>,
    disk_probe: &dyn Fn() -> u8,
    peer_device: &str,
) -> Result<RetentionOutcome> {
    let policy = config.retention.to_core_policy();
    let dry_run = config.retention.dry_run;
    let mut deleter = |pkg_ref: &Path| {
        retention_delete_source(store, seen, pkg_ref, "retention_deleted", peer_device)
    };
    evaluate_and_apply(store, &policy, dry_run, now, disk_probe, &mut deleter)
}

/// The outcome of a manual [`delete_confirmed_packages`] call: the ids actually
/// deleted, and the ids rejected (each with a human reason). Serialized directly
/// as the `POST /api/delete` response (task 10).
#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteReport {
    /// Outbound-row ids whose source capture file was removed from disk.
    pub deleted: Vec<i64>,
    /// Ids that were not deleted, each with the reason (not confirmed, unknown,
    /// no live source, or a delete error).
    pub rejected: Vec<DeleteRejection>,
}

/// One rejected id from [`delete_confirmed_packages`], with a human-readable
/// reason for the web UI to surface next to the row.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteRejection {
    pub id: i64,
    pub reason: String,
}

/// Delete the source capture files for a set of CONFIRMED outbound packages, by
/// outbound-row id — the web "Delete selected" action's chokepoint (task 10).
///
/// Shares the EXACT per-package deleter retention uses
/// ([`retention_delete_source`]); the only difference is the audit outcome tag
/// (`deleted_manual` here vs `retention_deleted`). Each id is verified to be in
/// state `confirmed` BEFORE anything is touched — an unknown or non-confirmed id
/// is rejected with a reason and never reaches disk. Deletion of anything not
/// `confirmed` is impossible by construction: the same invariant retention
/// relies on, enforced here at the id-lookup gate. Every safety property of the
/// shared deleter (audit-before-delete, TOCTOU stat guard, honest no-op vs real
/// removal) applies unchanged.
pub fn delete_confirmed_packages(
    store: &StandaloneSyncStore,
    seen: &SeenStore,
    ids: &[i64],
    peer_device: &str,
) -> Result<DeleteReport> {
    let mut report = DeleteReport::default();
    for &id in ids {
        let Some(row) = store.get_outbound(id)? else {
            report.rejected.push(DeleteRejection {
                id,
                reason: "unknown package".to_string(),
            });
            continue;
        };
        if row.state != OutboundState::Confirmed {
            report.rejected.push(DeleteRejection {
                id,
                reason: "not confirmed".to_string(),
            });
            continue;
        }
        let pkg_ref = PathBuf::from(&row.package_ref);
        match retention_delete_source(store, seen, &pkg_ref, "deleted_manual", peer_device) {
            Ok(DeleteOutcome::Removed) => {
                tracing::info!(id, package_ref = %row.package_ref, "manual delete removed confirmed source");
                report.deleted.push(id);
            }
            Ok(DeleteOutcome::SkippedNoop) => {
                // The row is confirmed, but there is no live source to remove
                // (already deleted, superseded by a re-enqueue, or gone
                // out-of-band). Honest no-op, surfaced as a per-id reject.
                report.rejected.push(DeleteRejection {
                    id,
                    reason: "no live source to delete (already removed or superseded)".to_string(),
                });
            }
            Err(error) => {
                tracing::error!(id, %error, "manual delete failed for confirmed package");
                report.rejected.push(DeleteRejection {
                    id,
                    reason: format!("delete failed: {error}"),
                });
            }
        }
    }
    Ok(report)
}

/// Spawn the config-driven retention timer. Each tick runs a full
/// evaluate-and-apply pass on a blocking thread (SQLite + fs), then logs the
/// outcome.
///
/// The retention config is sourced from the [`watch`] receiver every pass
/// (`rx.borrow().clone()`), so a live edit pushed via [`Agent::retention_tx`]
/// (tasks 9/10) takes effect on the next tick without an agent restart — the new
/// interval, policy, and dry-run flag are all picked up. Only the retention knobs
/// live on the channel; the capture directories to disk-probe are fixed for the
/// process lifetime, so they are resolved once up front.
///
/// Two exit paths: aborted on [`Agent::shutdown`] (holds only `Arc` clones, so
/// there is nothing to drain), or — gracefully — the loop breaks when the
/// `retention_tx` sender is dropped (`rx.changed()` errors).
fn spawn_retention_task(
    store: Arc<StandaloneSyncStore>,
    seen: Arc<SeenStore>,
    config: Config,
    mut retention_rx: watch::Receiver<RetentionConfig>,
    retention_log: Arc<Mutex<VecDeque<RetentionRunRecord>>>,
    peer_device: String,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // Capture volumes are static across retention edits; resolve once.
        let capture_dirs = config.capture_dirs_resolved();
        loop {
            let retention = retention_rx.borrow().clone();
            let interval = retention.interval();
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                changed = retention_rx.changed() => {
                    if changed.is_err() {
                        // Sender dropped = agent shutting down.
                        tracing::debug!("retention loop stopped (sender dropped)");
                        break;
                    }
                    tracing::info!("retention config updated; applying on next pass");
                    continue; // re-borrow the new config at the top of the loop
                }
            }

            // One pass with the current retention config. `run_retention_once`
            // reads its policy/dry-run from the config's `retention` table, so
            // splice the live retention knobs onto the static base config.
            // Snapshot the policy label + dry-run before the config is moved into
            // the blocking closure, so the pass record can name them either way.
            let policy_label = crate::config_edit::policy_str(&retention.policy).to_string();
            let dry_run_cfg = retention.dry_run;
            let store = Arc::clone(&store);
            let seen = Arc::clone(&seen);
            let mut pass_config = config.clone();
            pass_config.retention = retention;
            let dirs = capture_dirs.clone();
            let peer_device = peer_device.clone();
            let res = tokio::task::spawn_blocking(move || {
                // Probe every capture volume and take the MAX usage: disk
                // pressure on any one watched directory should trigger the gate.
                let disk_probe = move || dirs.iter().map(|d| disk_usage_pct(d)).max().unwrap_or(0);
                run_retention_once(
                    &pass_config,
                    &store,
                    &seen,
                    Utc::now(),
                    &disk_probe,
                    &peer_device,
                )
            })
            .await;
            // Log AND record the pass for the web status page (task 10). The
            // record maps `RetentionOutcome`'s path Vecs verbatim; a failed /
            // panicked tick records its error into `errors` with empty lists.
            let record = match res {
                Ok(Ok(outcome)) => {
                    tracing::info!(
                        dry_run = outcome.dry_run,
                        eligible = outcome.eligible.len(),
                        deleted = outcome.deleted.len(),
                        would_warn_disk_pressure = outcome.would_warn_disk_pressure,
                        "retention tick complete"
                    );
                    let mut errors = Vec::new();
                    if outcome.would_warn_disk_pressure {
                        errors.push("disk still at/over cap after pass".to_string());
                    }
                    RetentionRunRecord {
                        at: now_iso(),
                        dry_run: outcome.dry_run,
                        policy: policy_label,
                        deleted: outcome
                            .deleted
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect(),
                        would_delete: outcome
                            .eligible
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect(),
                        errors,
                    }
                }
                Ok(Err(error)) => {
                    tracing::error!(%error, "retention tick failed");
                    RetentionRunRecord {
                        at: now_iso(),
                        dry_run: dry_run_cfg,
                        policy: policy_label,
                        deleted: Vec::new(),
                        would_delete: Vec::new(),
                        errors: vec![format!("retention tick failed: {error}")],
                    }
                }
                Err(error) => {
                    tracing::error!(%error, "retention tick task panicked");
                    RetentionRunRecord {
                        at: now_iso(),
                        dry_run: dry_run_cfg,
                        policy: policy_label,
                        deleted: Vec::new(),
                        would_delete: Vec::new(),
                        errors: vec![format!("retention tick task panicked: {error}")],
                    }
                }
            };
            {
                let mut log = retention_log.lock().expect("retention_log mutex poisoned");
                log.push_front(record);
                log.truncate(50);
            }
        }
    })
}

/// Enumerate eligible capture files already present under `dir` (recursive),
/// sorted for deterministic order.
pub fn backlog_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(dir).sort_by_file_name() {
        let entry = entry.with_context(|| format!("walk backlog dir {}", dir.display()))?;
        if entry.file_type().is_file() && watcher::is_eligible(entry.path()) {
            out.push(entry.path().to_path_buf());
        }
    }
    Ok(out)
}

/// Guard that keeps the non-blocking log writer's worker thread alive for the
/// process lifetime. Drop it (at process exit) to flush.
pub struct LogGuard(#[allow(dead_code)] tracing_appender::non_blocking::WorkerGuard);

/// Default tracing filter when `ATHENAEUM_LOG` is unset. Perseus's own modules
/// (and the shared `athenaeum_core::sync` engine) stay at `info`; iroh's
/// transport/relay/blob internals and its network-probe dependencies
/// (`portmapper`, `netwatch`, `noq_udp`, `net_report`) are quieted to `warn`.
/// Left at `info` they bury the handful of real sync events — a single evening
/// run produced ~71k `iroh::socket::transports` span-close events (>99% of log
/// volume). Raise any of them explicitly via `ATHENAEUM_LOG`, which overrides
/// this default entirely, e.g. `ATHENAEUM_LOG=info,iroh=debug`.
const DEFAULT_LOG_FILTER: &str = "info,iroh=warn,iroh_relay=warn,iroh_blobs=warn,net_report=warn,portmapper=warn,netwatch=warn,noq_udp=warn";

/// Initialize tracing: rolling JSONL files under `<data_dir>/logs` with a
/// `perseus.*` filename prefix (daily rotation, 14 files retained), plus a
/// human line to stderr for foreground / journald. `ATHENAEUM_LOG` overrides
/// the default filter entirely (shared convention with the desktop/web hosts);
/// the default ([`DEFAULT_LOG_FILTER`]) keeps our modules at `info` while
/// quieting iroh's verbose transport/probe internals to `warn`.
///
/// Built directly on `tracing-appender` rather than `athenaeum_core::logging`:
/// that module hardcodes a `Process` enum (Desktop/Web only, no Perseus prefix)
/// and resolves its directory from `ATHENAEUM_*` env vars, neither of which fits
/// the config-driven `<data_dir>/logs` + `perseus.` prefix Perseus needs.
pub fn init_logging(log_dir: &Path) -> Result<LogGuard> {
    use tracing_appender::rolling::{Builder, Rotation};
    use tracing_subscriber::fmt::format::FmtSpan;
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    std::fs::create_dir_all(log_dir)
        .with_context(|| format!("create log dir {}", log_dir.display()))?;

    let file_appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("perseus")
        .filename_suffix("jsonl")
        .max_log_files(14)
        .build(log_dir)
        .context("build rolling log appender")?;
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_env("ATHENAEUM_LOG")
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));

    let file_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(non_blocking);
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(std::io::stderr);

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init()
        .context("install tracing subscriber")?;

    Ok(LogGuard(guard))
}

#[cfg(test)]
mod rel_path_tests {
    use super::*;

    /// A config whose single capture dir is `dir` (no per-dir label).
    fn single_root_config(dir: &str) -> Config {
        let mut c = Config::fallback();
        c.capture_dir = Some(PathBuf::from(dir));
        c.capture_dirs = Vec::new();
        c
    }

    /// A config with several capture dirs (each stable file gets a per-dir label).
    fn multi_root_config(dirs: &[&str]) -> Config {
        let mut c = Config::fallback();
        c.capture_dir = None;
        c.capture_dirs = dirs.iter().map(PathBuf::from).collect();
        c
    }

    /// Task 6: with a single capture dir, `rel_path` is the file's path relative
    /// to that dir — forward-slash, no label.
    #[test]
    fn rel_path_is_relative_to_capture_dir() {
        let cfg = single_root_config("/data/astro");
        let rel = compute_rel_path(
            &cfg,
            Path::new("/data/astro"),
            Path::new("/data/astro/M31/2026-07-10/L_0001.fits"),
        );
        assert_eq!(rel, "M31/2026-07-10/L_0001.fits");
    }

    /// Task 6: with more than one capture dir, `rel_path` is prefixed with a
    /// sanitized per-dir label (the capture dir basename) so identically-named
    /// files from different roots don't collide on the receiver.
    #[test]
    fn rel_path_gets_root_label_when_multi_root() {
        let cfg = multi_root_config(&["/data/astro", "/mnt/backup"]);
        let rel = compute_rel_path(
            &cfg,
            Path::new("/mnt/backup"),
            Path::new("/mnt/backup/M31/L_0001.fits"),
        );
        assert_eq!(rel, "backup/M31/L_0001.fits");
    }

    /// A file directly under the (single) capture dir yields a bare filename —
    /// the pre-Task-6 behavior, unchanged for the flat-directory case.
    #[test]
    fn rel_path_of_top_level_file_is_the_filename() {
        let cfg = single_root_config("/data/astro");
        let rel = compute_rel_path(&cfg, Path::new("/data/astro"), Path::new("/data/astro/L_0001.fits"));
        assert_eq!(rel, "L_0001.fits");
    }

    /// A capture-dir basename with characters outside `[a-z0-9._-]` is sanitized
    /// to a single safe segment (spaces → `-`, uppercase → lowercase).
    #[test]
    fn multi_root_label_is_sanitized() {
        let cfg = multi_root_config(&["/data/astro", "/mnt/My Backup"]);
        let rel = compute_rel_path(
            &cfg,
            Path::new("/mnt/My Backup"),
            Path::new("/mnt/My Backup/sub/x.fits"),
        );
        assert_eq!(rel, "my-backup/sub/x.fits");
    }

    /// The computed `rel_path` is always `validate_rel_path`-clean (forward-slash,
    /// no `..`/root), even in the multi-root labelled case.
    #[test]
    fn computed_rel_path_is_validate_clean() {
        let cfg = multi_root_config(&["/data/astro", "/mnt/backup"]);
        let rel = compute_rel_path(
            &cfg,
            Path::new("/mnt/backup"),
            Path::new("/mnt/backup/M31/L_0001.fits"),
        );
        athenaeum_core::package::validate_rel_path(&rel).expect("rel_path must be wire-clean");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// IMPORTANT #1 (review): an existing device key with group/world-readable
    /// permissions (backup restore, loose umask) must be tightened back to
    /// 0600 on load, not silently trusted.
    #[test]
    fn insecure_existing_key_permissions_are_tightened_on_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("device_key");
        std::fs::write(&path, [7u8; 32]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let key = load_or_create_device_key(&path).expect("load existing key");
        assert_eq!(key, [7u8; 32], "the key's bytes must be unchanged");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "permissions must be tightened to 0600 on load");
    }

    /// A key already at 0600 is left alone (no spurious rewrite/log noise).
    #[test]
    fn already_secure_key_permissions_are_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("device_key");
        std::fs::write(&path, [9u8; 32]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let key = load_or_create_device_key(&path).expect("load existing key");
        assert_eq!(key, [9u8; 32]);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// A freshly created key is written 0600 (existing behavior, guarded here
    /// too so a regression in either path is caught).
    #[test]
    fn newly_created_key_is_0600() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("device_key");
        assert!(!path.exists());

        load_or_create_device_key(&path).expect("create key");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// Review finding (non-fatal web bind): a port conflict on the OPTIONAL
    /// status page must NOT halt the agent's primary function (watch + sync).
    /// `bind_and_spawn_web` therefore logs-and-continues rather than propagating
    /// — proven here by occupying an ephemeral port first, then handing the same
    /// address to the binder and asserting it yields no task handle (agent
    /// startup would sail past this) instead of erroring out.
    #[tokio::test]
    async fn web_bind_conflict_is_non_fatal() {
        // Hold a real listener on an OS-assigned free port for the whole test so
        // the address is genuinely occupied when the binder tries it.
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupy ephemeral port");
        let addr = occupied
            .local_addr()
            .expect("read occupied addr")
            .to_string();

        let handle = bind_and_spawn_web(&addr, axum::Router::new()).await;
        assert!(
            handle.is_none(),
            "a runtime bind failure must be swallowed (no web task), not propagated"
        );
    }

    /// Positive control: a free port binds and yields a live task handle, so the
    /// success path is exercised alongside the failure path (guards against a
    /// binder that returns `None` unconditionally). The task is aborted so the
    /// bound port is released promptly.
    #[tokio::test]
    async fn web_bind_success_yields_task() {
        let handle = bind_and_spawn_web("127.0.0.1:0", axum::Router::new()).await;
        let task = handle.expect("binding a free ephemeral port must spawn the server task");
        task.abort();
    }
}

/// End-to-end retention tests over real files: the confirmed → source-capture
/// mapping (`perseus_seen.package_ref` join) plus the fs-deleter + audit-history
/// side effects, in dry-run and real-delete mode. These exercise
/// [`run_retention_once`] — the exact composition the hourly tick runs.
#[cfg(test)]
mod retention_tests {
    use super::*;

    use athenaeum_core::package::MANIFEST_FILENAME;
    use athenaeum_core::sharing::types::FrameReceipt;
    use athenaeum_core::sync::{HistoryQuery, OutboundState, SyncStore};

    use crate::config::RetentionPolicy as CfgPolicy;

    const PEER: NodeId = [3u8; 32];

    /// The sync-peer hex these tests stamp on audit rows — the same value a
    /// transfer row would carry (`node_id_hex(&peer)`), NOT this node's own id.
    fn peer_hex() -> String {
        node_id_hex(&PEER)
    }

    /// Test-only store wrapper that fails every `append_history` call,
    /// delegating everything else to the wrapped real store. Proves the
    /// audit-before-delete refusal (review IMPORTANT #1): if the audit can't be
    /// persisted, the source file must survive.
    struct FailingAppendHistoryStore<'a>(&'a StandaloneSyncStore);

    impl SyncStore for FailingAppendHistoryStore<'_> {
        fn enqueue(&self, package_ref: &str, peer: NodeId) -> Result<i64> {
            self.0.enqueue(package_ref, peer)
        }
        fn set_state(&self, id: i64, s: OutboundState) -> Result<()> {
            self.0.set_state(id, s)
        }
        fn set_last_error(&self, id: i64, err: Option<&str>) -> Result<()> {
            self.0.set_last_error(id, err)
        }
        fn set_next_retry_at(&self, id: i64, at: Option<&str>) -> Result<()> {
            self.0.set_next_retry_at(id, at)
        }
        fn set_wire_package_id(&self, id: i64, wire_id: &str) -> Result<()> {
            self.0.set_wire_package_id(id, wire_id)
        }
        fn bump_attempts(&self, id: i64) -> Result<u32> {
            self.0.bump_attempts(id)
        }
        fn non_terminal(&self) -> Result<Vec<OutboundRow>> {
            self.0.non_terminal()
        }
        fn get_outbound(&self, id: i64) -> Result<Option<OutboundRow>> {
            self.0.get_outbound(id)
        }
        fn confirmed(&self) -> Result<Vec<OutboundRow>> {
            self.0.confirmed()
        }
        fn confirm(&self, id: i64, receipts: &[FrameReceipt]) -> Result<()> {
            self.0.confirm(id, receipts)
        }
        fn append_history(&self, _h: HistoryRow) -> Result<()> {
            Err(anyhow::anyhow!("simulated append_history failure"))
        }
        fn search_history(&self, q: HistoryQuery) -> Result<Vec<HistoryRow>> {
            self.0.search_history(q)
        }
    }

    /// A config over fresh tempdirs with an existing `capture_dir` (validate()
    /// requires it). Returns the tempdir guard so the dirs outlive the test.
    fn setup() -> (tempfile::TempDir, Config, StandaloneSyncStore, SeenStore) {
        let tmp = tempfile::tempdir().unwrap();
        let capture = tmp.path().join("capture");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&capture).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let toml = format!(
            "capture_dir=\"{}\"\ndata_dir=\"{}\"\npairing_ticket=\"t\"\nmode=\"auto\"\n[retention]\npolicy=\"on_confirm\"\ndry_run=true\n",
            capture.display(),
            data.display()
        );
        let config = Config::from_toml_str(&toml).unwrap();
        // Store + seen share the one perseus.db file (WAL) — the production wiring.
        let store = StandaloneSyncStore::open(config.db_path()).unwrap();
        let seen = SeenStore::open(config.db_path()).unwrap();
        (tmp, config, store, seen)
    }

    /// Hand-build a one-file package (no FITS parsing) whose manifest carries a
    /// minimal `frame_meta`, and return its directory (== the `package_ref`).
    fn make_package(packages_dir: &Path, src: &Path, object: &str) -> PathBuf {
        let uuid = Uuid::new_v4().to_string();
        let filename = src.file_name().unwrap().to_str().unwrap().to_string();
        let byte_size = std::fs::metadata(src).unwrap().len();
        let record = ManifestRecord {
            v: MANIFEST_VERSION,
            frame_uuid: uuid.clone(),
            origin_catalog_uuid: uuid.clone(),
            origin_device: "test-device".to_string(),
            payload_kind: PayloadKind::RawFrame,
            rel_path: filename,
            byte_size,
            xxh3: "0".repeat(16),
            frame_meta: serde_json::json!({ "object": object }),
            analysis: None,
            app_version: "test".to_string(),
            project: None,
        };
        let pkg_dir = packages_dir.join(&uuid);
        write_package(&pkg_dir, vec![(src.to_path_buf(), record)]).unwrap();
        pkg_dir
    }

    /// Register a source file → package: enqueue the outbound row, link the seen
    /// row (with the file's REAL current `(size, mtime)`, exactly as
    /// `record_seen` does in production — this is what makes the TOCTOU
    /// stat-match guard meaningful in these tests), return `(package_ref,
    /// outbound_id)`.
    fn register(
        config: &Config,
        store: &StandaloneSyncStore,
        seen: &SeenStore,
        src: &Path,
    ) -> (PathBuf, i64) {
        let pkg = make_package(&config.packages_dir(), src, "M42");
        let id = store.enqueue(&pkg.to_string_lossy(), PEER).unwrap();
        let meta = std::fs::metadata(src).unwrap();
        let mtime_ms = crate::seen::mtime_millis(meta.modified().ok());
        seen.mark_enqueued(src, meta.len(), mtime_ms, &pkg.to_string_lossy())
            .unwrap();
        (pkg, id)
    }

    fn history_outcome_count(store: &StandaloneSyncStore, outcome: &str) -> usize {
        store
            .search_history(HistoryQuery {
                filename: None,
                object: None,
                direction: None,
                peer: None,
                project: None,
                package_id: None,
                limit: 1000,
            })
            .unwrap()
            .iter()
            .filter(|h| h.outcome == outcome)
            .count()
    }

    fn history_deleted_count(store: &StandaloneSyncStore) -> usize {
        history_outcome_count(store, "retention_deleted")
    }

    /// Real-delete mode: a confirmed package's source is deleted, an audit row is
    /// written, the seen row is stamped deleted; the unconfirmed source is
    /// untouched and stays resolvable.
    #[test]
    fn run_retention_once_deletes_only_confirmed_sources() {
        let (_tmp, mut config, store, seen) = setup();
        // Test-only: bypass the config validator's dry_run guard to exercise the
        // real deletion path (production still refuses dry_run = false).
        config.retention.dry_run = false;

        let s1 = config.capture_dirs_resolved()[0].join("light-0001.fits");
        let s2 = config.capture_dirs_resolved()[0].join("light-0002.fits");
        let su = config.capture_dirs_resolved()[0].join("light-0003.fits");
        std::fs::write(&s1, b"aaaa").unwrap();
        std::fs::write(&s2, b"bbbb").unwrap();
        std::fs::write(&su, b"cccc").unwrap();

        let (pkg1, id1) = register(&config, &store, &seen, &s1);
        let (_pkg2, id2) = register(&config, &store, &seen, &s2);
        let (pku, _idu) = register(&config, &store, &seen, &su);

        store.confirm(id1, &[]).unwrap();
        store.confirm(id2, &[]).unwrap();

        let probe = || 0u8;
        let outcome =
            run_retention_once(&config, &store, &seen, Utc::now(), &probe, &peer_hex()).unwrap();

        assert_eq!(outcome.deleted.len(), 2, "both confirmed sources deleted");
        assert!(
            !s1.exists() && !s2.exists(),
            "confirmed source files removed"
        );
        assert!(su.exists(), "the unconfirmed source is never touched");
        assert_eq!(
            history_deleted_count(&store),
            2,
            "one audit row per deletion"
        );
        assert_eq!(
            seen.source_for_package(&pkg1.to_string_lossy()).unwrap(),
            None,
            "a deleted source no longer resolves"
        );
        assert_eq!(
            seen.source_for_package(&pku.to_string_lossy())
                .unwrap()
                .map(|l| l.path),
            Some(su.clone()),
            "the unconfirmed package's source stays live and resolvable"
        );
    }

    /// Regression: the `retention_deleted` audit row is stamped with the SYNC
    /// PEER (the same hex a transfer row carries), NOT the manifest's
    /// `origin_device` — which is this node's OWN id (the owner's screenshot bug
    /// showed self in this column).
    #[test]
    fn audit_row_stamps_sync_peer_not_self() {
        let (_tmp, mut config, store, seen) = setup();
        config.retention.dry_run = false;

        let s1 = config.capture_dirs_resolved()[0].join("light-0001.fits");
        std::fs::write(&s1, b"aaaa").unwrap();
        let (_pkg1, id1) = register(&config, &store, &seen, &s1);
        store.confirm(id1, &[]).unwrap();

        run_retention_once(&config, &store, &seen, Utc::now(), &|| 0u8, &peer_hex()).unwrap();

        let rows = store
            .search_history(HistoryQuery {
                filename: None,
                object: None,
                direction: None,
                peer: None,
                project: None,
                package_id: None,
                limit: 1000,
            })
            .unwrap();
        let audit: Vec<_> = rows
            .iter()
            .filter(|h| h.outcome == "retention_deleted")
            .collect();
        assert_eq!(audit.len(), 1);
        assert_eq!(
            audit[0].peer_device,
            peer_hex(),
            "the audit row stamps the sync peer, not self"
        );
        // `make_package` stamps `origin_device = \"test-device\"` (this node's own
        // id in production) — asserting we did NOT fall back to the manifest value.
        assert_ne!(audit[0].peer_device, "test-device");
    }

    /// Dry-run mode (the default): nothing is deleted, no audit rows, files and
    /// seen linkage intact — but the pass still reports what it would delete.
    #[test]
    fn run_retention_once_dry_run_reports_but_deletes_nothing() {
        let (_tmp, config, store, seen) = setup(); // dry_run = true from TOML

        let s1 = config.capture_dirs_resolved()[0].join("light-0001.fits");
        std::fs::write(&s1, b"aaaa").unwrap();
        let (pkg1, id1) = register(&config, &store, &seen, &s1);
        store.confirm(id1, &[]).unwrap();

        let outcome =
            run_retention_once(&config, &store, &seen, Utc::now(), &|| 0u8, &peer_hex()).unwrap();

        assert!(outcome.dry_run);
        assert_eq!(outcome.eligible.len(), 1, "reports the confirmed candidate");
        assert!(outcome.deleted.is_empty(), "dry-run deletes nothing");
        assert!(s1.exists(), "the file remains on disk");
        assert_eq!(history_deleted_count(&store), 0, "no audit rows in dry-run");
        assert_eq!(
            seen.source_for_package(&pkg1.to_string_lossy())
                .unwrap()
                .map(|l| l.path),
            Some(s1),
            "the seen linkage is untouched"
        );
    }

    /// The hard invariant at the Perseus composition level: an UNCONFIRMED
    /// source is never deleted, even in real mode under maximum disk pressure,
    /// and never surfaces as eligible.
    #[test]
    fn unconfirmed_source_never_surfaces_even_under_disk_pressure() {
        let (_tmp, mut config, store, seen) = setup();
        config.retention.policy = CfgPolicy::DiskPct;
        config.retention.disk_max_pct = 50;
        config.retention.dry_run = false; // real mode — still must not delete

        let s1 = config.capture_dirs_resolved()[0].join("light-0001.fits");
        std::fs::write(&s1, b"aaaa").unwrap();
        // Enqueued + linked but NEVER confirmed.
        let (pkg1, _id1) = register(&config, &store, &seen, &s1);

        let outcome =
            run_retention_once(&config, &store, &seen, Utc::now(), &|| 99u8, &peer_hex()).unwrap();

        assert!(outcome.eligible.is_empty(), "unconfirmed never eligible");
        assert!(outcome.deleted.is_empty());
        assert!(
            outcome.would_warn_disk_pressure,
            "full disk + nothing to free warns"
        );
        assert!(s1.exists(), "the unconfirmed source survives a full disk");
        assert_eq!(history_deleted_count(&store), 0);
        assert_eq!(
            seen.source_for_package(&pkg1.to_string_lossy())
                .unwrap()
                .map(|l| l.path),
            Some(s1),
            "the source is still live (never deleted)"
        );
    }

    // ── review fixes (audit-before-delete + TOCTOU guard) ────────────────────

    /// IMPORTANT #1(a): an unreadable manifest must not silence the audit trail
    /// — the delete still proceeds via a minimal fallback `retention_deleted`
    /// row rather than zero history rows.
    #[test]
    fn manifest_unreadable_falls_back_to_minimal_audit_row() {
        let (_tmp, mut config, store, seen) = setup();
        config.retention.dry_run = false;

        let s1 = config.capture_dirs_resolved()[0].join("light-0001.fits");
        std::fs::write(&s1, b"aaaa").unwrap();
        let (pkg1, id1) = register(&config, &store, &seen, &s1);
        store.confirm(id1, &[]).unwrap();

        // Corrupt the package: remove its manifest so `read_manifest` fails.
        std::fs::remove_file(pkg1.join(MANIFEST_FILENAME)).unwrap();

        let outcome =
            run_retention_once(&config, &store, &seen, Utc::now(), &|| 0u8, &peer_hex()).unwrap();

        assert_eq!(
            outcome.deleted.len(),
            1,
            "the delete still proceeds via a fallback audit row"
        );
        assert!(!s1.exists());
        assert_eq!(
            history_deleted_count(&store),
            1,
            "a minimal fallback retention_deleted row is written even without a readable manifest"
        );
    }

    /// IMPORTANT #1(b): if even the fallback audit row can't be persisted, the
    /// delete must be refused entirely this tick — the source survives and the
    /// seen linkage stays live. Proven with an injected store whose
    /// `append_history` always fails.
    #[test]
    fn fallback_audit_insert_failure_prevents_delete() {
        let (_tmp, config, store, seen) = setup();

        let s1 = config.capture_dirs_resolved()[0].join("light-0001.fits");
        std::fs::write(&s1, b"aaaa").unwrap();
        let (pkg1, id1) = register(&config, &store, &seen, &s1);
        store.confirm(id1, &[]).unwrap();

        let failing = FailingAppendHistoryStore(&store);
        let result =
            retention_delete_source(&failing, &seen, &pkg1, "retention_deleted", &peer_hex());

        assert!(
            result.is_err(),
            "an unpersistable audit must refuse the delete"
        );
        assert!(
            s1.exists(),
            "the source survives when the audit can't be written"
        );
        assert_eq!(
            seen.source_for_package(&pkg1.to_string_lossy())
                .unwrap()
                .map(|l| l.path),
            Some(s1),
            "the seen linkage stays live — nothing was actually removed"
        );
    }

    /// IMPORTANT #2: a concurrent re-enqueue that rewrites the source path
    /// between confirmation and the retention pass must abort the delete — the
    /// stat-match guard compares the file's current `(size, mtime)` against
    /// what was recorded at enqueue time.
    #[test]
    fn source_changed_since_confirmation_is_skipped() {
        let (_tmp, mut config, store, seen) = setup();
        config.retention.dry_run = false;

        let s1 = config.capture_dirs_resolved()[0].join("light-0001.fits");
        std::fs::write(&s1, b"aaaa").unwrap();
        let (_pkg1, id1) = register(&config, &store, &seen, &s1);
        store.confirm(id1, &[]).unwrap();

        // Simulate a concurrent re-write at the same path: a NEW, unconfirmed
        // file lands here after confirmation but before retention runs.
        std::fs::write(&s1, b"brand-new-unconfirmed-content").unwrap();

        let outcome =
            run_retention_once(&config, &store, &seen, Utc::now(), &|| 0u8, &peer_hex()).unwrap();

        assert!(
            outcome.deleted.is_empty(),
            "a stat-mismatched source must not be deleted"
        );
        assert!(s1.exists(), "the rewritten (unconfirmed) content survives");
        assert_eq!(
            history_deleted_count(&store),
            0,
            "no audit row for a guard-skipped delete"
        );
    }

    /// Minor #3: a package whose linkage was already handled by an earlier
    /// pass (already `deleted_at`-stamped) is a legitimate no-op — it must not
    /// be recounted as a deletion or write a duplicate audit row.
    #[test]
    fn already_deleted_linkage_skip_writes_no_history_and_is_not_counted_deleted() {
        let (_tmp, mut config, store, seen) = setup();
        config.retention.dry_run = false;

        let s1 = config.capture_dirs_resolved()[0].join("light-0001.fits");
        std::fs::write(&s1, b"aaaa").unwrap();
        let (_pkg1, id1) = register(&config, &store, &seen, &s1);
        store.confirm(id1, &[]).unwrap();

        // Simulate this package's linkage already handled by an earlier pass.
        seen.mark_deleted(&s1).unwrap();

        let outcome =
            run_retention_once(&config, &store, &seen, Utc::now(), &|| 0u8, &peer_hex()).unwrap();

        assert!(
            outcome.deleted.is_empty(),
            "an already-handled package must never be recounted as deleted"
        );
        assert_eq!(
            history_deleted_count(&store),
            0,
            "no duplicate audit row is written"
        );
        assert!(
            s1.exists(),
            "the file — already logically gone — is left untouched again"
        );
    }

    /// Minor #4: a source removed out-of-band (not by retention) must be
    /// stamped dead so it stops being offered, but must NOT produce a
    /// `retention_deleted` audit row — retention never touched it.
    #[test]
    fn out_of_band_removed_source_is_stamped_without_audit_row() {
        let (_tmp, mut config, store, seen) = setup();
        config.retention.dry_run = false;

        let s1 = config.capture_dirs_resolved()[0].join("light-0001.fits");
        std::fs::write(&s1, b"aaaa").unwrap();
        let (pkg1, id1) = register(&config, &store, &seen, &s1);
        store.confirm(id1, &[]).unwrap();

        // Out-of-band removal: something other than retention deleted the file.
        std::fs::remove_file(&s1).unwrap();

        let outcome =
            run_retention_once(&config, &store, &seen, Utc::now(), &|| 0u8, &peer_hex()).unwrap();

        assert!(
            outcome.deleted.is_empty(),
            "a file gone out-of-band is not counted as a retention deletion"
        );
        assert_eq!(
            history_deleted_count(&store),
            0,
            "no retention_deleted row for an out-of-band removal"
        );
        assert_eq!(
            history_outcome_count(&store, "retention_source_missing"),
            0,
            "the outcome tag is a log field, not a history row — no row of any kind is written"
        );
        assert_eq!(
            seen.source_for_package(&pkg1.to_string_lossy()).unwrap(),
            None,
            "the dead linkage is stamped so it stops being offered"
        );
    }
}

/// Sync 2C (Task 7): the multi-target send — one engine per target, each built
/// package fanned out to every engine, with per-target failure isolation.
#[cfg(test)]
mod multi_target_tests {
    use super::*;

    use athenaeum_core::sharing::loopback::{LoopbackNetwork, LoopbackTransport};
    use athenaeum_core::sharing::types::{FrameReceipt, ReceiptOutcome, TransportEvent};
    use athenaeum_core::sharing::SharingTransport;
    use athenaeum_core::sync::store::SyncStore;
    use athenaeum_core::sync::{
        OutboundState, PackageCleanupSink, SharedPackageCleanup, SyncEngine,
    };

    /// Hand-build a one-file package (real payload + hash so a receiver can fetch
    /// it) and return its directory.
    fn make_pkg(dir: &Path, uuid: &str, name: &str) -> PathBuf {
        let src = dir.join(name);
        std::fs::write(&src, b"payload-bytes-0123456789").unwrap();
        let byte_size = std::fs::metadata(&src).unwrap().len();
        let xxh3 = package::xxh3_full_file(&src).unwrap();
        let record = ManifestRecord {
            v: MANIFEST_VERSION,
            frame_uuid: uuid.to_string(),
            origin_catalog_uuid: uuid.to_string(),
            origin_device: "test-device".to_string(),
            payload_kind: PayloadKind::RawFrame,
            rel_path: name.to_string(),
            byte_size,
            xxh3,
            frame_meta: serde_json::json!({ "object": "M42" }),
            analysis: None,
            app_version: "test".to_string(),
            project: None,
        };
        let pkg_dir = dir.join(format!("pkg-{uuid}"));
        write_package(&pkg_dir, vec![(src, record)]).unwrap();
        pkg_dir
    }

    /// As [`make_pkg`], but also returns the [`PackageAnnounce`] `write_package`
    /// produced (its `package_id` is the serve tag) — for the shared-node test,
    /// which serves real payloads onto the one node's store without a network.
    fn make_announced_pkg(
        dir: &Path,
        uuid: &str,
        name: &str,
    ) -> (PathBuf, athenaeum_core::sharing::types::PackageAnnounce) {
        let src = dir.join(name);
        std::fs::write(&src, b"payload-bytes-0123456789").unwrap();
        let byte_size = std::fs::metadata(&src).unwrap().len();
        let xxh3 = package::xxh3_full_file(&src).unwrap();
        let record = ManifestRecord {
            v: MANIFEST_VERSION,
            frame_uuid: uuid.to_string(),
            origin_catalog_uuid: uuid.to_string(),
            origin_device: "test-device".to_string(),
            payload_kind: PayloadKind::RawFrame,
            rel_path: name.to_string(),
            byte_size,
            xxh3,
            frame_meta: serde_json::json!({ "object": "M42" }),
            analysis: None,
            app_version: "test".to_string(),
            project: None,
        };
        let pkg_dir = dir.join(format!("apkg-{uuid}"));
        let announce = write_package(&pkg_dir, vec![(src, record)]).unwrap();
        (pkg_dir, announce)
    }

    /// Spawn a reactive receiver on `endpoint`: fetch every announced package and
    /// ack every frame as `Ingested`, so the sender's row reaches `Confirmed`.
    fn spawn_receiver(endpoint: Arc<LoopbackTransport>, dest_root: PathBuf) {
        tokio::spawn(async move {
            let mut events = endpoint.events().await;
            let mut n = 0usize;
            while let Some(event) = events.recv().await {
                let TransportEvent::AnnounceReceived { from, announce } = event else {
                    continue;
                };
                n += 1;
                let dest = dest_root.join(format!("fetch-{n}"));
                if endpoint
                    .fetch(from, &announce, &dest, athenaeum_core::sharing::noop_fetch_sink())
                    .await
                    .is_ok()
                {
                    if let Ok(records) = read_manifest(&dest) {
                        let receipts: Vec<FrameReceipt> = records
                            .iter()
                            .map(|r| FrameReceipt {
                                frame_uuid: r.frame_uuid.clone(),
                                xxh3: r.xxh3.clone(),
                                outcome: ReceiptOutcome::Ingested,
                            })
                            .collect();
                        let _ = endpoint.ack(from, &announce.package_id, receipts).await;
                    }
                }
            }
        });
    }

    async fn wait_until<F: FnMut() -> bool>(mut pred: F) {
        for _ in 0..500 {
            if pred() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("condition never became true");
    }

    /// One built package fans out to EVERY target engine: both peers receive and
    /// confirm it, so the shared store ends with one confirmed outbound row per
    /// target (proving per-target delivery).
    // multi_thread runtime: a current_thread test starves its single OS thread
    // under host load (2 engines + 2 receivers + mutex loopback), the ~2/12 flake
    // this test carried (audit TEST-9). Two workers give the loopback room to run.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fan_out_delivers_to_every_target() {
        let tmp = tempfile::tempdir().unwrap();
        let net = LoopbackNetwork::new();

        let recv_a = Arc::new(net.endpoint());
        let a_id = recv_a.start().await.unwrap().node_id;
        spawn_receiver(recv_a.clone(), tmp.path().join("ra"));
        let recv_b = Arc::new(net.endpoint());
        let b_id = recv_b.start().await.unwrap().node_id;
        spawn_receiver(recv_b.clone(), tmp.path().join("rb"));

        let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
        let engine_a = Arc::new(SyncEngine::spawn(
            Arc::clone(&store) as Arc<dyn SyncStore>,
            Arc::new(net.endpoint()),
            a_id,
        ));
        let engine_b = Arc::new(SyncEngine::spawn(
            Arc::clone(&store) as Arc<dyn SyncStore>,
            Arc::new(net.endpoint()),
            b_id,
        ));

        let pkg = make_pkg(tmp.path(), "uuid-1", "frame.fits");
        let (first_id, delivered) =
            enqueue_package_to_all(&[Arc::clone(&engine_a), Arc::clone(&engine_b)], &pkg).await;
        assert!(first_id.is_some(), "at least one target accepted the package");
        assert_eq!(delivered, 2, "the package reached both targets");

        // Both per-target rows reach Confirmed (one per peer).
        wait_until(|| {
            store
                .all_outbound(100)
                .unwrap()
                .iter()
                .filter(|r| r.state == OutboundState::Confirmed)
                .count()
                == 2
        })
        .await;
        let confirmed: Vec<NodeId> = store
            .all_outbound(100)
            .unwrap()
            .iter()
            .filter(|r| r.state == OutboundState::Confirmed)
            .map(|r| r.peer)
            .collect();
        assert!(confirmed.contains(&a_id) && confirmed.contains(&b_id), "one confirmed row per target peer");

        engine_a.shutdown().await;
        engine_b.shutdown().await;
    }

    /// A per-target enqueue failure never drops the others (spec §8): engine B is
    /// shut down before the fan-out, so its `enqueue_package` fails, but engine A
    /// still gets the package — the store ends with exactly one outbound row, for
    /// peer A.
    #[tokio::test]
    async fn fan_out_isolates_a_failing_target() {
        let tmp = tempfile::tempdir().unwrap();
        let net = LoopbackNetwork::new();

        let recv_a = Arc::new(net.endpoint());
        let a_id = recv_a.start().await.unwrap().node_id;
        // A live receiver so A's row can even confirm; not required for the
        // isolation assertion, which only needs the row to exist.
        spawn_receiver(recv_a.clone(), tmp.path().join("ra"));
        let b_id = [0xBBu8; 32];

        let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
        let engine_a = Arc::new(SyncEngine::spawn(
            Arc::clone(&store) as Arc<dyn SyncStore>,
            Arc::new(net.endpoint()),
            a_id,
        ));
        let engine_b = Arc::new(SyncEngine::spawn(
            Arc::clone(&store) as Arc<dyn SyncStore>,
            Arc::new(net.endpoint()),
            b_id,
        ));
        // Kill B's worker: its `enqueue_package` will now error.
        engine_b.shutdown().await;

        let pkg = make_pkg(tmp.path(), "uuid-1", "frame.fits");
        let (first_id, delivered) =
            enqueue_package_to_all(&[Arc::clone(&engine_a), Arc::clone(&engine_b)], &pkg).await;
        assert!(first_id.is_some(), "the live target still accepted the package");
        assert_eq!(delivered, 1, "only the live target accepted it — the dead one is skipped, not fatal");

        // Exactly one outbound row exists, and it belongs to peer A.
        let rows = store.all_outbound(100).unwrap();
        assert_eq!(rows.len(), 1, "the failed target enqueued nothing");
        assert_eq!(rows[0].peer, a_id, "the surviving row is for the live target");

        engine_a.shutdown().await;
    }

    /// The regression this whole change exists for: when target B is offline as
    /// target A confirms, the SHARED payload must NOT be deleted — otherwise B's
    /// retry re-serves a manifest-only collection and B silently never gets the
    /// frame. Two engines share one coordinator + one package dir; B's peer has
    /// no receiver (never acks → its row stays non-terminal), so after A confirms
    /// the payload must survive. Cancelling B (terminal) then lets the
    /// coordinator clean exactly once.
    #[tokio::test]
    async fn offline_target_keeps_shared_payload_until_it_terminalizes() {
        let tmp = tempfile::tempdir().unwrap();
        let net = LoopbackNetwork::new();

        // Target A: a live receiver that acks → its row confirms.
        let recv_a = Arc::new(net.endpoint());
        let a_id = recv_a.start().await.unwrap().node_id;
        spawn_receiver(recv_a.clone(), tmp.path().join("ra"));
        // Target B: a peer with NO endpoint → announce never reaches it, so B's
        // row never confirms (stays non-terminal, retrying) — "offline".
        let b_id = [0xBBu8; 32];

        let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("sync.db")).unwrap());
        let coord = Arc::new(SharedPackageCleanup::new());
        let engine_a = Arc::new(SyncEngine::spawn_with_sink(
            Arc::clone(&store) as Arc<dyn SyncStore>,
            Arc::new(net.endpoint()),
            a_id,
            Arc::clone(&coord) as Arc<dyn PackageCleanupSink>,
        ));
        let engine_b = Arc::new(SyncEngine::spawn_with_sink(
            Arc::clone(&store) as Arc<dyn SyncStore>,
            Arc::new(net.endpoint()),
            b_id,
            Arc::clone(&coord) as Arc<dyn PackageCleanupSink>,
        ));

        let pkg = make_pkg(tmp.path(), "uuid-off", "frame.fits");
        // The fan-out reached both targets → expected = 2.
        coord.register(&pkg, 2);
        let _id_a = engine_a.enqueue_package(&pkg).await.unwrap();
        let id_b = engine_b.enqueue_package(&pkg).await.unwrap();

        // A confirms. Under the OLD code this deleted the shared payload.
        wait_until(|| {
            store
                .all_outbound(100)
                .unwrap()
                .iter()
                .any(|r| r.peer == a_id && r.state == OutboundState::Confirmed)
        })
        .await;
        // Give any (erroneous) cleanup a chance to run.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            pkg.join("frame.fits").exists(),
            "the shared payload MUST survive A's confirm while B is still offline"
        );

        // B terminalizes (cancel) → both targets terminal → clean exactly once.
        engine_b.cancel(id_b).await.unwrap();
        wait_until(|| !pkg.join("frame.fits").exists()).await;
        assert!(
            !pkg.join("frame.fits").exists(),
            "once B is terminal too the coordinator frees the shared payload"
        );

        engine_a.shutdown().await;
        engine_b.shutdown().await;
    }

    /// `start_with_transports` spawns exactly one engine per injected target.
    #[tokio::test]
    async fn start_with_transports_spawns_one_engine_per_target() {
        let tmp = tempfile::tempdir().unwrap();
        let cap = tmp.path().join("cap");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&cap).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let toml = format!(
            "capture_dir=\"{}\"\ndata_dir=\"{}\"\npairing_ticket=\"t\"\nmode=\"auto\"\n[retention]\npolicy=\"keep_everything\"\ndry_run=true\n",
            cap.display(),
            data.display()
        );
        let config = Config::from_toml_str(&toml).unwrap();

        let net = LoopbackNetwork::new();
        let a_id = [1u8; 32];
        let b_id = [2u8; 32];
        let sender = Arc::new(net.endpoint());
        let sender_id = sender.node_id();
        let transports: Vec<(NodeId, Arc<dyn SharingTransport>)> = vec![
            (a_id, Arc::new(net.endpoint())),
            (b_id, Arc::new(net.endpoint())),
        ];

        let agent = Agent::start_with_transports(config, transports, sender_id, false, None)
            .await
            .expect("agent starts with two targets");
        assert_eq!(agent.engine_count(), 2, "one engine per injected target");
        agent.shutdown().await;
    }

    /// Task 4 (iroh hardening): the production shape is ONE `SharedIrohNode` with
    /// a `Role::Out` handle per target — instead of one endpoint per target,
    /// which self-collided on the relay (a relay keeps only one connection per
    /// node id, so a two-destination batch evicted its own peers). Loopback
    /// injection bypasses the node, so "one node" is unobservable through
    /// `start_with_transports`; this pins the production invariant DIRECTLY over a
    /// real relay-disabled node (node-handle introspection, the form the brief
    /// blesses), staying hermetic and low-footprint — no background-dialing
    /// engines, so it never destabilises a neighbouring timing test:
    ///   1. two `Role::Out` handles both `start()` onto the SAME node identity —
    ///      i.e. every target rides the ONE endpoint (the anti-self-collision
    ///      property `Agent::start` now relies on);
    ///   2. each handle takes its OWN inbound event stream (the Task-2 demux fans
    ///      the node's single stream out per handle, so N per-peer send engines
    ///      coexist on one node without stealing each other's acks); and
    ///   3. both handles serve real payloads onto the node's ONE blob store
    ///      (local, no network) — the store is shared and a serve on one target
    ///      never trips the other's prefix-scoped tags.
    /// End-to-end fan-out delivery/confirm is already covered hermetically by
    /// `fan_out_delivers_to_every_target` (loopback); the novel Task-4 property is
    /// the single shared node, which this test pins.
    #[tokio::test]
    async fn one_shared_node_serves_every_target() {
        use athenaeum_core::sharing::iroh::node::{Role, SharedIrohNode};
        use iroh::RelayMode;

        let tmp = tempfile::tempdir().unwrap();
        // ONE node (relay-disabled, hermetic); its device key + advisory lock
        // live under this data dir — the same key the hub registers at sign-in.
        let node = SharedIrohNode::bind(tmp.path(), RelayMode::Disabled)
            .await
            .expect("bind the one shared node");

        // (1) One Out handle per target — both are views on the ONE endpoint.
        let h1: Arc<dyn SharingTransport> = node.handle(Role::Out);
        let h2: Arc<dyn SharingTransport> = node.handle(Role::Out);
        assert_eq!(
            h1.start().await.unwrap().node_id,
            node.node_id(),
            "target 1 rides the one node's endpoint"
        );
        assert_eq!(
            h2.start().await.unwrap().node_id,
            node.node_id(),
            "target 2 rides the SAME endpoint (no per-target endpoint)"
        );

        // (2) Each target handle gets its OWN inbound event stream.
        let _e1 = h1.events().await;
        let _e2 = h2.events().await;

        // (3) Both targets serve onto the node's single blob store (local, no net).
        let (pkg1, ann1) = make_announced_pkg(tmp.path(), "uuid-a", "a.fits");
        let (pkg2, ann2) = make_announced_pkg(tmp.path(), "uuid-b", "b.fits");
        h1.serve(&ann1, &pkg1, None)
            .await
            .expect("target 1 serves onto the shared store");
        h2.serve(&ann2, &pkg2, None)
            .await
            .expect("target 2 serves onto the SAME shared store");

        node.shutdown().await;
    }
}

/// Task 3: [`build_batch_package`] bundles N capture files into ONE package with
/// a record per surviving file, dropping any file that vanished / won't parse
/// (never fatal) and erroring only when nothing is buildable.
#[cfg(test)]
mod batch_package_tests {
    use super::*;

    use athenaeum_core::fits_writer::keywords::{FrameKind, HeaderBuilder};
    use athenaeum_core::fits_writer::write_fits_f32;

    /// Write a minimal, parseable single-frame FITS at `path` via core's writer.
    fn write_fixture_fits(path: &Path, object: &str) {
        let cards = HeaderBuilder::new(FrameKind::Light)
            .object(object)
            .exptime(60.0)
            .filter("Ha")
            .instrume("TestCam")
            .build()
            .expect("build header");
        let data = vec![0.0f32; 8 * 8];
        write_fits_f32(path, 8, 8, 1, &data, &cards).expect("write fixture fits");
    }

    /// Three parseable FITS under one temp capture dir, plus a config whose
    /// `data_dir` (and thus `packages_dir()`) is a sibling temp dir. The temp
    /// tree is intentionally kept (`TempDir::keep`) so the written package
    /// survives for the assertion instead of being reaped when the guard drops.
    fn three_capture_files() -> (Config, PathBuf, Vec<PathBuf>) {
        let tmp = tempfile::tempdir().unwrap().keep();
        let capture = tmp.join("capture");
        let data = tmp.join("data");
        std::fs::create_dir_all(&capture).unwrap();
        std::fs::create_dir_all(&data).unwrap();

        let files: Vec<PathBuf> = ["a.fits", "b.fits", "c.fits"]
            .iter()
            .map(|name| {
                let p = capture.join(name);
                write_fixture_fits(&p, "M42");
                p
            })
            .collect();

        let toml = format!(
            "capture_dir=\"{}\"\ndata_dir=\"{}\"\npairing_ticket=\"t\"\nmode=\"auto\"\n[retention]\npolicy=\"keep_everything\"\ndry_run=true\n",
            capture.display(),
            data.display()
        );
        let config = Config::from_toml_str(&toml).unwrap();
        (config, capture, files)
    }

    #[test]
    fn build_batch_package_bundles_all_present_files() {
        let (config, cap, files) = three_capture_files();
        let input: Vec<_> = files.iter().map(|f| (cap.clone(), f.clone())).collect();
        let (pkg_dir, included) = build_batch_package(&config, &input, &"aa".repeat(32)).unwrap();
        assert_eq!(included, files, "every present file is included, in order");
        let recs = athenaeum_core::package::read_manifest(&pkg_dir).unwrap();
        assert_eq!(recs.len(), 3); // one manifest, 3 records
    }

    #[test]
    fn build_batch_package_drops_vanished_file() {
        let (config, cap, files) = three_capture_files();
        std::fs::remove_file(&files[1]).unwrap(); // one gone before build
        let input: Vec<_> = files.iter().map(|f| (cap.clone(), f.clone())).collect();
        let (pkg_dir, included) = build_batch_package(&config, &input, &"aa".repeat(32)).unwrap();
        // Only the two survivors are included — the vanished file is NOT (so the
        // batcher never marks it seen); order is preserved.
        assert_eq!(included, vec![files[0].clone(), files[2].clone()]);
        assert_eq!(
            athenaeum_core::package::read_manifest(&pkg_dir).unwrap().len(),
            2
        );
    }
}
