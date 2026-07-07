//! The running agent: durable sync store, iroh transport, sync engine, and the
//! capture watcher wired together.
//!
//! [`Agent`] is transport-injectable. Production ([`Agent::start`]) builds an
//! [`IrohTransport`] from a persisted device key and derives the peer node id
//! from the pairing ticket. Tests ([`Agent::start_with_transport`]) inject an
//! in-process loopback transport and a known peer, exercising the exact same
//! enqueue/package/engine path without a network.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use athenaeum_core::fits_parser::{parse_fits_with_header, parse_xisf};
use athenaeum_core::package::{
    self, read_manifest, write_package, ManifestRecord, PayloadKind, MANIFEST_VERSION,
};
use athenaeum_core::sharing::iroh::{random_secret, BlobStore, IrohTransport};
use athenaeum_core::sharing::types::NodeId;
use athenaeum_core::sharing::SharingTransport;
use athenaeum_core::sync::store::{StandaloneSyncStore, SyncStore};
use athenaeum_core::sync::{
    evaluate_and_apply, Direction, HistoryRow, OutboundRow, RetentionOutcome, SyncEngine,
    SyncEngineHandle,
};
use chrono::{DateTime, Utc};
use iroh::RelayMode;
use iroh_tickets::endpoint::EndpointTicket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::Config;
use crate::seen::SeenStore;
use crate::watcher::{self, WatcherHandle};

/// Lowercase-hex (64 char) rendering of a 32-byte node id — the same format the
/// sync store uses for its `peer`/`origin_device` columns.
fn node_id_hex(id: &NodeId) -> String {
    let mut s = String::with_capacity(64);
    for b in id {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

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

/// Load the persisted 32-byte device secret, creating it (mode 0600) on first
/// run. On every load of an existing key, permissions are re-checked and
/// tightened back to 0600 if a group/world bit has crept in (backup restore,
/// a loose umask, manual copy) — the identity secret must never be readable
/// by anyone but the service user.
pub fn load_or_create_device_key(path: &Path) -> Result<[u8; 32]> {
    if path.exists() {
        tighten_permissions_if_needed(path)?;
        let bytes = std::fs::read(path)
            .with_context(|| format!("read device key {}", path.display()))?;
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
    let meta = std::fs::metadata(path)
        .with_context(|| format!("stat device key {}", path.display()))?;
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
    std::fs::write(path, secret)
        .with_context(|| format!("write device key {}", path.display()))?;
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
pub fn build_package_for_file(
    config: &Config,
    file_path: &Path,
    origin_device: &str,
) -> Result<PathBuf> {
    let frame = parse_frame(file_path)?;
    let frame_uuid = Uuid::new_v4().to_string();
    let filename = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("capture file has no valid name: {}", file_path.display()))?
        .to_string();
    let byte_size = std::fs::metadata(file_path)
        .with_context(|| format!("stat capture file {}", file_path.display()))?
        .len();
    let xxh3 = package::xxh3_full_file(file_path)?;

    let record = ManifestRecord {
        v: MANIFEST_VERSION,
        frame_uuid: frame_uuid.clone(),
        // No producer catalog: anchor origin identity to the minted frame uuid.
        origin_catalog_uuid: frame_uuid.clone(),
        origin_device: origin_device.to_string(),
        payload_kind: PayloadKind::RawFrame,
        rel_path: filename,
        byte_size,
        xxh3,
        frame_meta: serde_json::to_value(&frame).context("serialize frame_meta")?,
        analysis: None,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    };

    let pkg_dir = config.packages_dir().join(&frame_uuid);
    write_package(&pkg_dir, vec![(file_path.to_path_buf(), record)])
        .with_context(|| format!("write package for {}", file_path.display()))?;
    Ok(pkg_dir)
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

/// A running capture agent. Owns the sync engine, the durable store, the
/// stat-aware seen store, and (optionally) the capture watcher + its enqueue
/// consumer.
pub struct Agent {
    config: Config,
    store: Arc<StandaloneSyncStore>,
    seen: Arc<SeenStore>,
    engine: Arc<SyncEngineHandle>,
    origin_device: String,
    watcher: Option<WatcherHandle>,
    enqueue_task: Option<JoinHandle<()>>,
    retention_task: Option<JoinHandle<()>>,
}

impl Agent {
    /// Start a production agent: persistent device key, iroh transport (default
    /// relays), peer derived from the pairing ticket. `watch` arms the capture
    /// watcher (true for `run`, false for `enqueue-backlog`).
    pub async fn start(config: Config, watch: bool) -> Result<Self> {
        std::fs::create_dir_all(&config.data_dir)
            .with_context(|| format!("create data dir {}", config.data_dir.display()))?;

        // Parse the pairing ticket exactly once: derive both the peer NodeId
        // and its dialable address, then register the address directly via
        // `add_peer` — `add_peer_ticket` would re-parse the same string.
        let ticket = parse_ticket(&config.pairing_ticket)?;
        let peer_addr = ticket.endpoint_addr().clone();
        let peer: NodeId = *peer_addr.id.as_bytes();

        let secret = load_or_create_device_key(&config.device_key_path())?;

        let transport = IrohTransport::new(
            secret,
            RelayMode::Default,
            BlobStore::Fs(config.data_dir.clone()),
        )
        .await
        .context("build iroh transport")?;
        transport.add_peer(peer_addr);
        let node_id = transport.node_id();
        tracing::info!(
            node_id = %node_id_hex(&node_id),
            peer = %node_id_hex(&peer),
            "iroh transport ready"
        );

        let transport: Arc<dyn SharingTransport> = Arc::new(transport);
        Self::start_with_transport(config, transport, peer, node_id, watch).await
    }

    /// Start an agent over a caller-supplied transport + peer. This is the
    /// injection seam the e2e test uses to run against a loopback transport; the
    /// production path routes through here after building the iroh transport.
    #[doc(hidden)]
    pub async fn start_with_transport(
        config: Config,
        transport: Arc<dyn SharingTransport>,
        peer: NodeId,
        node_id: NodeId,
        watch: bool,
    ) -> Result<Self> {
        std::fs::create_dir_all(&config.data_dir)
            .with_context(|| format!("create data dir {}", config.data_dir.display()))?;

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
        let engine = Arc::new(SyncEngine::spawn(
            Arc::clone(&store) as Arc<dyn SyncStore>,
            transport,
            peer,
        ));
        let origin_device = node_id_hex(&node_id);

        // Retention is now ACTIVE (task A8) — but dry-run stays the enforced
        // default until the M-Perseus-MVP soak gate lifts (config rejects
        // dry_run = false), so no files are deleted in this build.
        tracing::info!(
            retention_policy = ?config.retention.policy,
            dry_run = config.retention.dry_run,
            interval_secs = config.retention.interval_secs,
            keep_days = config.retention.keep_days,
            disk_max_pct = config.retention.disk_max_pct,
            "retention active"
        );

        let (watcher, enqueue_task, retention_task) = if watch {
            let (stable_tx, stable_rx) = mpsc::channel::<PathBuf>(64);
            let watcher = watcher::spawn_watcher(
                config.capture_dir.clone(),
                config.stability(),
                config.poll_interval(),
                stable_tx,
                Arc::clone(&seen),
            )?;
            let enqueue_task = spawn_enqueue_consumer(
                stable_rx,
                Arc::clone(&engine),
                Arc::clone(&seen),
                config.clone(),
                origin_device.clone(),
            );
            let retention_task = spawn_retention_task(
                Arc::clone(&store),
                Arc::clone(&seen),
                config.clone(),
            );
            (Some(watcher), Some(enqueue_task), Some(retention_task))
        } else {
            (None, None, None)
        };

        Ok(Self {
            config,
            store,
            seen,
            engine,
            origin_device,
            watcher,
            enqueue_task,
            retention_task,
        })
    }

    /// Build a package for `file_path` and enqueue it for sending; returns the
    /// durable outbound row id. Used by `enqueue-backlog` and reused by the
    /// watcher consumer. Records the file's current `(size, mtime)` in the seen
    /// store once the durable `Queued` row exists, so a later restart never
    /// re-packages this exact, unchanged file.
    pub async fn enqueue_file(&self, file_path: &Path) -> Result<i64> {
        let pkg_dir = build_package_for_file(&self.config, file_path, &self.origin_device)?;
        let id = self.engine.enqueue_package(&pkg_dir).await?;
        tracing::info!(id, path = %file_path.display(), "enqueued capture file");
        record_seen(&self.seen, file_path, &pkg_dir.to_string_lossy());
        Ok(id)
    }

    /// Live in-flight (non-terminal) outbound rows.
    pub fn status_snapshot(&self) -> Result<Vec<OutboundRow>> {
        self.engine.status_snapshot()
    }

    /// The durable store (test/introspection).
    pub fn store(&self) -> &Arc<StandaloneSyncStore> {
        &self.store
    }

    /// This agent's own device id (hex).
    pub fn origin_device(&self) -> &str {
        &self.origin_device
    }

    /// Gracefully stop the watcher, drain the enqueue consumer, and shut the
    /// engine down (awaiting its worker).
    pub async fn shutdown(self) {
        // The retention loop is an independent timer task with no channel to
        // close; abort it directly (it holds only Arc clones, nothing to drain).
        if let Some(t) = self.retention_task {
            t.abort();
        }
        if let Some(w) = self.watcher {
            w.shutdown().await;
        }
        // The watcher owned the stable-file sender; with it dropped the consumer
        // sees its channel close and exits.
        if let Some(t) = self.enqueue_task {
            let _ = t.await;
        }
        self.engine.shutdown().await;
    }

    /// Hard-kill: abort the watcher and enqueue-consumer tasks immediately (no
    /// graceful handshake), then drop this agent's engine handle. Once the
    /// aborted enqueue task's own handle clone is also gone, the engine's
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
        if let Some(w) = self.watcher {
            w.abort_for_test();
        }
        if let Some(t) = self.enqueue_task {
            t.abort();
        }
        if let Some(t) = self.retention_task {
            t.abort();
        }
        // `self.engine` (and `self.store`/`self.seen`) drop here, at end of scope.
    }
}

/// Record `path`'s current `(size, mtime)` and its `package_ref` in the seen
/// store. Best-effort: a failure here only means a possible harmless re-send on a
/// future restart — it must never fail the enqueue itself (the file is already
/// durably queued). The `package_ref` linkage is what retention later joins on to
/// map a confirmed package back to this source capture file.
fn record_seen(seen: &SeenStore, path: &Path, package_ref: &str) {
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

/// Spawn the task that turns stable capture files into enqueued packages.
fn spawn_enqueue_consumer(
    mut stable_rx: mpsc::Receiver<PathBuf>,
    engine: Arc<SyncEngineHandle>,
    seen: Arc<SeenStore>,
    config: Config,
    origin_device: String,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(path) = stable_rx.recv().await {
            match build_package_for_file(&config, &path, &origin_device) {
                Ok(pkg_dir) => match engine.enqueue_package(&pkg_dir).await {
                    Ok(id) => {
                        tracing::info!(id, path = %path.display(), "enqueued capture file");
                        record_seen(&seen, &path, &pkg_dir.to_string_lossy());
                    }
                    Err(error) => {
                        tracing::error!(%error, path = %path.display(), "enqueue failed")
                    }
                },
                Err(error) => {
                    tracing::error!(%error, path = %path.display(), "build package failed")
                }
            }
        }
        tracing::debug!("enqueue consumer stopped");
    })
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

/// The retention deleter (fs remove + audit history + seen-store update) for one
/// confirmed package. `pkg_ref` is `sync_outbound.package_ref` (the package
/// directory) — the abstract deletable subject core hands us; here we resolve it
/// back to the *original source capture file* and remove that.
///
/// Idempotent and safe by construction: if there is no *live* source linkage
/// (already retention-deleted, or the path was re-enqueued under a newer
/// package), it is a logged no-op — never an error, never a stray delete.
fn retention_delete_source(
    store: &StandaloneSyncStore,
    seen: &SeenStore,
    pkg_ref: &Path,
) -> Result<()> {
    let pkg_ref_str = pkg_ref.to_string_lossy();

    let Some(source) = seen.source_for_package(&pkg_ref_str)? else {
        tracing::warn!(
            package_ref = %pkg_ref.display(),
            "retention: no live source linkage for confirmed package; skipping (already deleted or superseded)"
        );
        return Ok(());
    };

    // Audit metadata comes from the package manifest (frame uuid / filename /
    // bytes / object) — never from the perseus_seen row, keeping core's history
    // schema the single source of truth for a transfer record.
    let records = read_manifest(pkg_ref).unwrap_or_else(|error| {
        tracing::warn!(package_ref = %pkg_ref.display(), %error, "retention: manifest unreadable; history rows will be minimal");
        Vec::new()
    });

    // Remove the source capture file (idempotent: absent = goal already met).
    if source.exists() {
        std::fs::remove_file(&source)
            .with_context(|| format!("retention delete source {}", source.display()))?;
    }

    // One audit row per manifest record (one-file packages → one row).
    for r in &records {
        let object = r
            .frame_meta
            .get("object")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        store.append_history(HistoryRow {
            frame_uuid: r.frame_uuid.clone(),
            filename: r.rel_path.clone(),
            object,
            peer_device: r.origin_device.clone(),
            direction: Direction::Sent,
            bytes: r.byte_size,
            started_at: now_iso(),
            finished_at: Some(now_iso()),
            outcome: "retention_deleted".to_string(),
        })?;
    }

    // Stamp the seen row deleted so it never surfaces to retention again.
    seen.mark_deleted(&source)?;
    Ok(())
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
) -> Result<RetentionOutcome> {
    let policy = config.retention.to_core_policy();
    let dry_run = config.retention.dry_run;
    let mut deleter = |pkg_ref: &Path| retention_delete_source(store, seen, pkg_ref);
    evaluate_and_apply(store, &policy, dry_run, now, disk_probe, &mut deleter)
}

/// Spawn the hourly (config-driven) retention timer. Each tick runs a full
/// evaluate-and-apply pass on a blocking thread (SQLite + fs), then logs the
/// outcome. Aborted on shutdown; holds only `Arc` clones, so there is nothing to
/// drain.
fn spawn_retention_task(
    store: Arc<StandaloneSyncStore>,
    seen: Arc<SeenStore>,
    config: Config,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let interval = config.retention.interval();
        loop {
            tokio::time::sleep(interval).await;
            let store = Arc::clone(&store);
            let seen = Arc::clone(&seen);
            let config = config.clone();
            let res = tokio::task::spawn_blocking(move || {
                let capture_dir = config.capture_dir.clone();
                let disk_probe = move || disk_usage_pct(&capture_dir);
                run_retention_once(&config, &store, &seen, Utc::now(), &disk_probe)
            })
            .await;
            match res {
                Ok(Ok(outcome)) => tracing::info!(
                    dry_run = outcome.dry_run,
                    eligible = outcome.eligible.len(),
                    deleted = outcome.deleted.len(),
                    would_warn_disk_pressure = outcome.would_warn_disk_pressure,
                    "retention tick complete"
                ),
                Ok(Err(error)) => tracing::error!(%error, "retention tick failed"),
                Err(error) => tracing::error!(%error, "retention tick task panicked"),
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

/// Initialize tracing: rolling JSONL files under `<data_dir>/logs` with a
/// `perseus.*` filename prefix (daily rotation, 14 files retained), plus a
/// human line to stderr for foreground / journald. `ATHENAEUM_LOG` overrides the
/// default `info` filter (shared convention with the desktop/web hosts).
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
        .unwrap_or_else(|_| EnvFilter::new("info"));

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
}

/// End-to-end retention tests over real files: the confirmed → source-capture
/// mapping (`perseus_seen.package_ref` join) plus the fs-deleter + audit-history
/// side effects, in dry-run and real-delete mode. These exercise
/// [`run_retention_once`] — the exact composition the hourly tick runs.
#[cfg(test)]
mod retention_tests {
    use super::*;

    use athenaeum_core::sync::{HistoryQuery, SyncStore};

    use crate::config::RetentionPolicy as CfgPolicy;

    const PEER: NodeId = [3u8; 32];

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
        };
        let pkg_dir = packages_dir.join(&uuid);
        write_package(&pkg_dir, vec![(src.to_path_buf(), record)]).unwrap();
        pkg_dir
    }

    /// Register a source file → package: enqueue the outbound row, link the seen
    /// row, return `(package_ref, outbound_id)`.
    fn register(
        config: &Config,
        store: &StandaloneSyncStore,
        seen: &SeenStore,
        src: &Path,
    ) -> (PathBuf, i64) {
        let pkg = make_package(&config.packages_dir(), src, "M42");
        let id = store.enqueue(&pkg.to_string_lossy(), PEER).unwrap();
        seen.mark_enqueued(src, 4, 1, &pkg.to_string_lossy()).unwrap();
        (pkg, id)
    }

    fn history_deleted_count(store: &StandaloneSyncStore) -> usize {
        store
            .search_history(HistoryQuery {
                filename: None,
                object: None,
                limit: 1000,
            })
            .unwrap()
            .iter()
            .filter(|h| h.outcome == "retention_deleted")
            .count()
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

        let s1 = config.capture_dir.join("light-0001.fits");
        let s2 = config.capture_dir.join("light-0002.fits");
        let su = config.capture_dir.join("light-0003.fits");
        std::fs::write(&s1, b"aaaa").unwrap();
        std::fs::write(&s2, b"bbbb").unwrap();
        std::fs::write(&su, b"cccc").unwrap();

        let (pkg1, id1) = register(&config, &store, &seen, &s1);
        let (_pkg2, id2) = register(&config, &store, &seen, &s2);
        let (pku, _idu) = register(&config, &store, &seen, &su);

        store.confirm(id1, &[]).unwrap();
        store.confirm(id2, &[]).unwrap();

        let probe = || 0u8;
        let outcome = run_retention_once(&config, &store, &seen, Utc::now(), &probe).unwrap();

        assert_eq!(outcome.deleted.len(), 2, "both confirmed sources deleted");
        assert!(!s1.exists() && !s2.exists(), "confirmed source files removed");
        assert!(su.exists(), "the unconfirmed source is never touched");
        assert_eq!(history_deleted_count(&store), 2, "one audit row per deletion");
        assert_eq!(
            seen.source_for_package(&pkg1.to_string_lossy()).unwrap(),
            None,
            "a deleted source no longer resolves"
        );
        assert_eq!(
            seen.source_for_package(&pku.to_string_lossy()).unwrap(),
            Some(su.clone()),
            "the unconfirmed package's source stays live and resolvable"
        );
    }

    /// Dry-run mode (the default): nothing is deleted, no audit rows, files and
    /// seen linkage intact — but the pass still reports what it would delete.
    #[test]
    fn run_retention_once_dry_run_reports_but_deletes_nothing() {
        let (_tmp, config, store, seen) = setup(); // dry_run = true from TOML

        let s1 = config.capture_dir.join("light-0001.fits");
        std::fs::write(&s1, b"aaaa").unwrap();
        let (pkg1, id1) = register(&config, &store, &seen, &s1);
        store.confirm(id1, &[]).unwrap();

        let outcome = run_retention_once(&config, &store, &seen, Utc::now(), &|| 0u8).unwrap();

        assert!(outcome.dry_run);
        assert_eq!(outcome.eligible.len(), 1, "reports the confirmed candidate");
        assert!(outcome.deleted.is_empty(), "dry-run deletes nothing");
        assert!(s1.exists(), "the file remains on disk");
        assert_eq!(history_deleted_count(&store), 0, "no audit rows in dry-run");
        assert_eq!(
            seen.source_for_package(&pkg1.to_string_lossy()).unwrap(),
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

        let s1 = config.capture_dir.join("light-0001.fits");
        std::fs::write(&s1, b"aaaa").unwrap();
        // Enqueued + linked but NEVER confirmed.
        let (pkg1, _id1) = register(&config, &store, &seen, &s1);

        let outcome = run_retention_once(&config, &store, &seen, Utc::now(), &|| 99u8).unwrap();

        assert!(outcome.eligible.is_empty(), "unconfirmed never eligible");
        assert!(outcome.deleted.is_empty());
        assert!(outcome.would_warn_disk_pressure, "full disk + nothing to free warns");
        assert!(s1.exists(), "the unconfirmed source survives a full disk");
        assert_eq!(history_deleted_count(&store), 0);
        assert_eq!(
            seen.source_for_package(&pkg1.to_string_lossy()).unwrap(),
            Some(s1),
            "the source is still live (never deleted)"
        );
    }
}
