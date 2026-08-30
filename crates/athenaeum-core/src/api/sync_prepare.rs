//! Preparation worker (transfer-prepare spec §3): stages a planned package into
//! `packages/<uuid>`, hashes it, writes the manifest, then hands the `queued`
//! row to the engine. One package at a time; cancellable; honest terminal on
//! failure; healed to `failed` after a restart.
//!
//! The split this module exists for: a `Send` used to copy and hash every
//! selected file *inside* the command, so the click did not become a row in the
//! Transfers list until the copy was over — minutes for a night of subs, with no
//! progress and nothing to cancel. Now
//! [`plan_selection_package`](crate::api::sync::plan_selection_package) only
//! reads the catalog and `stat`s the sources, the row is inserted `preparing`
//! with its full file manifest, and everything that touches bytes happens here.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::api::{db, ApiError};
use crate::events::{emit_event, ProgressEmitter};
use crate::package::{stage_payload, write_manifest, ManifestRecord, StageCancelled};
use crate::services::ServiceContext;
use crate::sharing::types::NodeId;
use crate::sync::engine::SyncEngineHandle;
use crate::sync::receiver::{SyncFileProgressEvent, SyncFinishedEvent, SyncProgressEvent};
use crate::sync::store::{CatalogSyncStore, SyncStore};
use crate::sync::{node_id_hex, Direction, OutboundState, SyncSenderRuntime};

/// Floor between two progress events of the same kind. Staging a package reads
/// at disk speed, so an unthrottled `on_progress` would emit one event per 4 MiB
/// chunk — hundreds a second on NVMe, all of them redundant to a progress bar.
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(300);

/// One catalog file whose full read the staging pass can bank as
/// `files.strong_hash` — if, and only if, the disk still matches the row when
/// the worker gets there (`disk_matches_row`, the staleness contract shared with
/// the master-hash pass and deep verify).
pub struct BankCandidate {
    pub file_id: i64,
    pub path: PathBuf,
    pub size: i64,
    pub modified_at: String,
    /// The package `rel_path` this file was planned under — how a banked hash is
    /// matched back to the manifest record that produced it.
    pub rel_path: String,
}

/// Everything one preparation needs: the durable row it belongs to, where it
/// stages, what it copies, and who to hand it to when it is whole.
pub struct PrepareJob {
    pub id: i64,
    pub peer: NodeId,
    pub pkg_dir: PathBuf,
    /// `(source, record)` with `record.xxh3` empty — filled by the worker.
    pub records: Vec<(PathBuf, ManifestRecord)>,
    pub bank: Vec<BankCandidate>,
    pub engine: Arc<SyncEngineHandle>,
    pub emitter: Option<Arc<dyn ProgressEmitter>>,
}

/// What the staging pass produced.
pub(crate) struct PrepareStats {
    pub(crate) files: usize,
    pub(crate) bytes: u64,
}

/// Why a preparation stopped short. The split matters to the user: a cancel is
/// something they asked for, a failure is something that went wrong — and only
/// one of the two deserves a reason on the row.
pub(crate) enum PrepareError {
    Cancelled,
    Failed {
        reason: String,
        /// `(rel_path, error)` of the single file that broke the run, when one
        /// file is to blame — so the detail view can name it instead of marking
        /// the whole batch failed.
        culprit: Option<(String, String)>,
    },
}

impl PrepareError {
    /// A one-line description for a caller that only needs the text. The worker
    /// itself never needs it (it has the parts it wants in hand); the
    /// synchronous staging seam the package-shape tests drive does.
    #[cfg(test)]
    pub(crate) fn describe(&self) -> String {
        match self {
            PrepareError::Cancelled => "cancelled".to_string(),
            PrepareError::Failed { reason, .. } => reason.clone(),
        }
    }
}

/// Fire-and-forget: acquires the single preparation slot, stages on a blocking
/// thread, then flips the row and drives it — or terminalizes it.
///
/// The cancel flag is registered SYNCHRONOUSLY, before the task is spawned, so a
/// cancel issued the instant the enqueue command returns can never arrive before
/// the flag exists.
pub fn spawn_prepare(ctx: Arc<ServiceContext>, sender: Arc<SyncSenderRuntime>, job: PrepareJob) {
    let flag = sender.prepare().register(job.id);
    let slot = sender.prepare().slot();
    // Copied out before `job` is moved into the staging closure, so every exit
    // path below — including the one that never gets to stage — can address the
    // row and the dir it was going to fill.
    let id = job.id;
    let peer = job.peer;
    let pkg_dir = job.pkg_dir.clone();
    let engine = Arc::clone(&job.engine);
    let emitter = job.emitter.clone();
    tokio::spawn(async move {
        let _permit = match slot.acquire_owned().await {
            Ok(p) => p,
            Err(e) => {
                // The runtime is going away, so this transfer will never be
                // staged. Say so on the row instead of leaving it `preparing`
                // forever — the same treatment the panic arm gives.
                tracing::error!(package_id = id, error = %e, "prepare slot closed");
                sender.prepare().finish(id);
                match sync_store(&ctx) {
                    Ok(store) => {
                        if claim_row(&store, id, &pkg_dir) {
                            terminalize(
                                &store,
                                id,
                                peer,
                                &pkg_dir,
                                OutboundState::Failed,
                                Some("preparation failed: preparation slot closed"),
                                "failed",
                                None,
                                emitter.as_deref(),
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(package_id = id, error = %format!("{e:#}"), "prepare: open store failed")
                    }
                }
                return;
            }
        };
        let ctx2 = Arc::clone(&ctx);
        let flag2 = Arc::clone(&flag);
        let started = Instant::now();
        let outcome = tokio::task::spawn_blocking(move || run_prepare(&ctx2, job, flag2)).await;
        // Released before any terminal bookkeeping: from here on the row is the
        // engine's or already terminal, and a cancel must fall through to the
        // engine rather than find a stale flag.
        sender.prepare().finish(id);
        let store = match sync_store(&ctx) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(package_id = id, error = %format!("{e:#}"), "prepare: open store failed");
                return;
            }
        };
        // ONE ownership check covering EVERY outcome, hoisted above the match on
        // purpose. Promote, cancel and fail all write a verdict, so a worker that
        // only checked before promoting would still overwrite someone else's
        // terminal with its own: a rewritten `last_error`, a second
        // `sync-finished` (a second notification), and file rows settled
        // `cancelled` under a batch now labelled `failed`.
        if !claim_row(&store, id, &pkg_dir) {
            return;
        }
        match outcome {
            Ok(Ok(stats)) => {
                // The staging loop reads the cancel flag at chunk boundaries
                // only, so one raised after the last chunk — while the manifest
                // was being written, while the pass banked its hashes, while
                // this task was waiting to be scheduled — reached nobody, and
                // the worker would go on to announce a package the user had
                // already stopped. This is the authoritative read, and it is
                // sound after `finish` above: `finish` only unregisters the
                // flag, this `Arc` is the same one every `cancel` stores
                // through, and the map lock the two share puts any store that
                // answered the user `true` before this load.
                if flag.load(Ordering::SeqCst) {
                    tracing::info!(package_id = id, "preparation cancelled after staging");
                    terminalize(
                        &store,
                        id,
                        peer,
                        &pkg_dir,
                        OutboundState::Cancelled,
                        None,
                        "cancelled",
                        None,
                        emitter.as_deref(),
                    );
                    return;
                }
                // The claim above and this promotion are two statements, so the
                // CAS carries the state the claim read into the WHERE clause: a
                // verdict that landed in between (a cancel routed to the engine)
                // makes it a no-op instead of a resurrection.
                match store.set_state_if(id, OutboundState::Preparing, OutboundState::Queued) {
                    Ok(true) => {}
                    Ok(false) => {
                        remove_staged_dir(id, &pkg_dir);
                        tracing::info!(
                            package_id = id,
                            "row moved on before promotion; discarding staged dir"
                        );
                        return;
                    }
                    Err(e) => {
                        tracing::error!(package_id = id, error = %format!("{e:#}"), "prepare: set queued failed");
                        return;
                    }
                }
                let duration_ms = started.elapsed().as_millis() as u64;
                if let Err(e) = store.append_sync_event(
                    Direction::Sent,
                    &id.to_string(),
                    "prepared",
                    Some(&format!(
                        "files={} bytes={} duration_ms={}",
                        stats.files, stats.bytes, duration_ms
                    )),
                ) {
                    tracing::warn!(package_id = id, error = %format!("{e:#}"), "prepare: journal failed");
                }
                tracing::info!(
                    package_id = id,
                    count = stats.files,
                    bytes = stats.bytes,
                    duration_ms,
                    "package prepared"
                );
                if let Err(e) = engine.drive(id).await {
                    tracing::error!(package_id = id, error = %format!("{e:#}"), "prepare: drive failed");
                }
            }
            Ok(Err(PrepareError::Cancelled)) => {
                tracing::info!(package_id = id, "preparation cancelled");
                terminalize(
                    &store,
                    id,
                    peer,
                    &pkg_dir,
                    OutboundState::Cancelled,
                    None,
                    "cancelled",
                    None,
                    emitter.as_deref(),
                );
            }
            Ok(Err(PrepareError::Failed { reason, culprit })) => {
                let msg = format!("preparation failed: {reason}");
                tracing::error!(package_id = id, error = %reason, "preparation failed");
                let culprit_ref = culprit.as_ref().map(|(r, e)| (r.as_str(), e.as_str()));
                terminalize(
                    &store,
                    id,
                    peer,
                    &pkg_dir,
                    OutboundState::Failed,
                    Some(&msg),
                    "failed",
                    culprit_ref,
                    emitter.as_deref(),
                );
            }
            Err(join) => {
                let msg = format!("preparation failed: worker panicked: {join}");
                tracing::error!(package_id = id, error = %join, "preparation worker panicked");
                terminalize(
                    &store,
                    id,
                    peer,
                    &pkg_dir,
                    OutboundState::Failed,
                    Some(&msg),
                    "failed",
                    None,
                    emitter.as_deref(),
                );
            }
        }
    });
}

/// The blocking half: stage every payload, then bank what the reads earned.
fn run_prepare(
    ctx: &ServiceContext,
    mut job: PrepareJob,
    flag: Arc<AtomicBool>,
) -> Result<PrepareStats, PrepareError> {
    let stats = stage_records(
        job.id,
        job.peer,
        &job.pkg_dir,
        &mut job.records,
        job.emitter.as_ref(),
        &flag,
    )?;
    match db(ctx) {
        Ok(handle) => bank_prepared_hashes(&handle.conn(), &job.records, &job.bank),
        Err(e) => tracing::warn!(
            package_id = job.id,
            error = %format!("{e:#}"),
            "prepare: catalog unavailable; strong_hash not banked"
        ),
    }
    Ok(stats)
}

/// Stage every planned payload into `pkg_dir`, fill each record's `xxh3` from the
/// single read that copy already paid for, and write the manifest LAST — so a
/// dir either has no manifest (and is nobody's to announce) or is whole.
///
/// Split out of [`run_prepare`] so it can be driven synchronously, without a row
/// or an engine, by the package-shape tests.
pub(crate) fn stage_records(
    id: i64,
    peer: NodeId,
    pkg_dir: &Path,
    records: &mut [(PathBuf, ManifestRecord)],
    emitter: Option<&Arc<dyn ProgressEmitter>>,
    flag: &AtomicBool,
) -> Result<PrepareStats, PrepareError> {
    let cancelled = || flag.load(Ordering::SeqCst);
    let total: u64 = records.iter().map(|(_, r)| r.byte_size).sum();
    let frame_count = records.len() as u32;
    let peer_hex = node_id_hex(&peer);

    let emit_batch = |bytes_done: u64| {
        if let Some(em) = emitter {
            emit_event(
                em.as_ref(),
                "sync-progress",
                &SyncProgressEvent {
                    package_id: id.to_string(),
                    direction: Direction::Sent,
                    stage: "preparing".to_string(),
                    peer_device: peer_hex.clone(),
                    frame_count,
                    project_id: None,
                    bytes_done: Some(bytes_done),
                    bytes_total: Some(total),
                },
            );
        }
    };
    let emit_file = |rel: &str, bytes_done: u64, bytes_total: u64| {
        if let Some(em) = emitter {
            emit_event(
                em.as_ref(),
                "sync-file-progress",
                &SyncFileProgressEvent {
                    package_id: id.to_string(),
                    peer_device: peer_hex.clone(),
                    file: rel.to_string(),
                    bytes_done,
                    bytes_total,
                },
            );
        }
    };

    std::fs::create_dir_all(pkg_dir).map_err(|e| {
        tracing::error!(package_id = id, path = %pkg_dir.display(), error = %e, "prepare: create package dir failed");
        PrepareError::Failed {
            reason: format!("create {}: {e}", pkg_dir.display()),
            culprit: None,
        }
    })?;

    let mut done_before: u64 = 0;
    let mut last_tick = Instant::now() - PROGRESS_MIN_INTERVAL;
    let mut hashes: Vec<String> = Vec::with_capacity(records.len());
    for (src, record) in records.iter() {
        let dest = pkg_dir.join(&record.rel_path);
        let size = record.byte_size;
        let rel = record.rel_path.clone();
        // Copied, not borrowed: `done_before` is fixed for the duration of this
        // file and is advanced after the closure is gone.
        let base = done_before;
        let mut file_last = Instant::now() - PROGRESS_MIN_INTERVAL;
        let mut on_progress = |file_done: u64| {
            if last_tick.elapsed() >= PROGRESS_MIN_INTERVAL {
                last_tick = Instant::now();
                emit_batch(base + file_done);
            }
            if file_last.elapsed() >= PROGRESS_MIN_INTERVAL {
                file_last = Instant::now();
                emit_file(&rel, file_done, size);
            }
        };
        let staged =
            stage_payload(src, &dest, size, &cancelled, &mut on_progress).map_err(|e| {
                if e.downcast_ref::<StageCancelled>().is_some() {
                    PrepareError::Cancelled
                } else {
                    tracing::error!(
                        package_id = id,
                        path = %src.display(),
                        error = %format!("{e:#}"),
                        "prepare: staging a payload failed"
                    );
                    PrepareError::Failed {
                        reason: format!("{}: {e:#}", record.rel_path),
                        culprit: Some((record.rel_path.clone(), format!("{e:#}"))),
                    }
                }
            })?;
        done_before += staged.bytes;
        // The file's terminal tick, emitted here rather than from `on_progress`:
        // a zero-byte payload reports no progress at all (there is no byte to
        // report), and a bar that never reaches its own total looks stuck.
        emit_file(&record.rel_path, staged.bytes, size);
        hashes.push(staged.xxh3);
    }

    for ((_, record), h) in records.iter_mut().zip(hashes.into_iter()) {
        record.xxh3 = h;
    }
    let manifest: Vec<ManifestRecord> = records.iter().map(|(_, r)| r.clone()).collect();
    write_manifest(pkg_dir, &manifest).map_err(|e| {
        tracing::error!(package_id = id, path = %pkg_dir.display(), error = %format!("{e:#}"), "prepare: write manifest failed");
        PrepareError::Failed {
            reason: format!("manifest: {e:#}"),
            culprit: None,
        }
    })?;

    emit_batch(total);
    Ok(PrepareStats {
        files: manifest.len(),
        bytes: total,
    })
}

/// Write the full hashes the staging pass computed into `files.strong_hash`, for
/// every candidate the disk still vouches for. Best-effort by design: the
/// package is the product, the banked hash a by-product.
pub(crate) fn bank_prepared_hashes(
    conn: &rusqlite::Connection,
    records: &[(PathBuf, ManifestRecord)],
    candidates: &[BankCandidate],
) {
    if candidates.is_empty() {
        return;
    }
    let by_rel: std::collections::HashMap<&str, &str> = records
        .iter()
        .map(|(_, r)| (r.rel_path.as_str(), r.xxh3.as_str()))
        .collect();
    let bank: Vec<(i64, String)> = candidates
        .iter()
        .filter_map(|c| {
            let h = by_rel.get(c.rel_path.as_str())?;
            crate::duplicates::backfill::disk_matches_row(&c.path, c.size, &c.modified_at)
                .then(|| (c.file_id, (*h).to_string()))
        })
        .collect();
    crate::api::sync::bank_manifest_hashes(conn, &bank);
}

/// Claim row `id` for a terminal (or promoting) write by the worker.
///
/// `true` — the row is still [`Preparing`](OutboundState::Preparing), so it is
/// still the worker's to speak for. `false` — someone else already wrote this
/// transfer's verdict (a cancel routed straight to the engine, a restart heal)
/// AND settled its per-file rows, so the worker throws away the dir it staged
/// and says nothing at all. A second terminal write here would be strictly
/// destructive: it overwrites `last_error` with a reason for an outcome the user
/// never saw, raises a second `sync-finished` (a second notification for one
/// transfer), and leaves file rows settled under one verdict beneath a batch
/// labelled with another.
///
/// The single gate for every arm of the worker's outcome match — promote,
/// cancel, fail, panic and slot-closed — so the asymmetry that made only the
/// promote path safe cannot come back.
///
/// It is a read, not a lock: a verdict landing between this check and the write
/// it guards would still slip through. The promote path therefore also carries
/// the state it read into its UPDATE
/// ([`set_state_if`](crate::sync::store::SyncStore::set_state_if)), so the one
/// write that could resurrect a stopped transfer is settled in a single
/// statement. The terminal arms need no such guard: their own write is what a
/// second owner would be overwriting, and the claim is what stops them.
fn claim_row(store: &CatalogSyncStore, id: i64, pkg_dir: &Path) -> bool {
    if still_preparing(store, id) {
        return true;
    }
    remove_staged_dir(id, pkg_dir);
    tracing::info!(
        package_id = id,
        "row already settled elsewhere; discarding staged dir"
    );
    false
}

/// Best-effort removal of a package dir the worker staged. An absent dir is the
/// normal case on several paths (nothing staged yet, an earlier owner already
/// swept it), so it is `debug!`, not `warn!`.
fn remove_staged_dir(id: i64, pkg_dir: &Path) {
    match std::fs::remove_dir_all(pkg_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(package_id = id, path = %pkg_dir.display(), "staged package dir already gone")
        }
        Err(e) => {
            tracing::warn!(package_id = id, path = %pkg_dir.display(), error = %e, "remove staged package dir failed")
        }
    }
}

/// Whether row `id` is still in [`Preparing`](OutboundState::Preparing) — the
/// worker's handover precondition.
///
/// A read failure answers `false`: refusing to promote a row whose state cannot
/// be confirmed leaves a `preparing` row for the next restart's heal to close,
/// which is recoverable; promoting one that was cancelled is not.
fn still_preparing(store: &CatalogSyncStore, id: i64) -> bool {
    match store.get_outbound(id) {
        Ok(Some(row)) if row.state == OutboundState::Preparing => true,
        Ok(Some(row)) => {
            tracing::warn!(
                package_id = id,
                state = row.state.as_str(),
                "row left preparing while it was being staged; not handing it to the engine"
            );
            false
        }
        Ok(None) => {
            tracing::warn!(package_id = id, "outbound row vanished during preparation");
            false
        }
        Err(e) => {
            tracing::error!(package_id = id, error = %format!("{e:#}"), "prepare: re-read row failed; not handing it to the engine");
            false
        }
    }
}

fn sync_store(ctx: &ServiceContext) -> Result<CatalogSyncStore, ApiError> {
    let dirs = crate::api::sync::sync_dirs(ctx)?;
    CatalogSyncStore::open(&dirs.db_path)
        .map_err(|e| ApiError::Internal(format!("open catalog sync store: {e:#}")))
}

/// Terminalize a row that never reached the engine: remove the partial dir,
/// stamp the state + reason, settle files, journal, emit `sync-finished`.
///
/// Every step is attempted even if an earlier one failed — a row left
/// non-terminal because its dir would not delete is far worse than a stale
/// directory.
#[allow(clippy::too_many_arguments)]
fn terminalize(
    store: &CatalogSyncStore,
    id: i64,
    peer: NodeId,
    pkg_dir: &Path,
    state: OutboundState,
    last_error: Option<&str>,
    outcome: &str,
    culprit: Option<(&str, &str)>,
    emitter: Option<&dyn ProgressEmitter>,
) {
    remove_staged_dir(id, pkg_dir);
    if let Err(e) = store.set_state(id, state) {
        tracing::error!(package_id = id, error = %format!("{e:#}"), "prepare: set terminal state failed");
    }
    if let Err(e) = store.set_last_error(id, last_error) {
        tracing::warn!(package_id = id, error = %format!("{e:#}"), "prepare: set last_error failed");
    }
    if let Err(e) = store.settle_files_terminal(id, outcome, culprit) {
        tracing::warn!(package_id = id, error = %format!("{e:#}"), "prepare: settle files failed");
    }
    let kind = if state == OutboundState::Cancelled {
        "cancelled"
    } else {
        "prepare_failed"
    };
    if let Err(e) = store.append_sync_event(Direction::Sent, &id.to_string(), kind, last_error) {
        tracing::warn!(package_id = id, error = %format!("{e:#}"), "prepare: journal failed");
    }
    if let Some(em) = emitter {
        emit_event(
            em,
            "sync-finished",
            &SyncFinishedEvent {
                package_id: id.to_string(),
                direction: Direction::Sent,
                outcome: outcome.to_string(),
                peer_device: node_id_hex(&peer),
                ok_count: 0,
                failed: Vec::new(),
                new_count: 0,
                duplicate_count: 0,
                project_id: None,
            },
        );
    }
}

/// Startup heal (spec §3.6): every `preparing` row → `failed`, partial dir
/// removed.
///
/// A preparation lives only in a running process — its staging thread and its
/// cancel flag are both gone after a restart, and the package dir it left behind
/// is a half-copy no announce could ever satisfy. So a `preparing` row found at
/// startup is not resumable work, it is a transfer that did not happen, and
/// saying so (with its Resend affordance intact) is the honest outcome.
pub fn heal_interrupted_preparations(ctx: &ServiceContext) -> Result<usize, ApiError> {
    let store = sync_store(ctx)?;
    let rows = store
        .non_terminal()
        .map_err(|e| ApiError::Internal(format!("list non-terminal outbound rows: {e:#}")))?;
    let mut healed = 0usize;
    for row in rows
        .into_iter()
        .filter(|r| r.state == OutboundState::Preparing)
    {
        tracing::warn!(
            package_id = row.id,
            path = %row.package_ref,
            "preparation interrupted by a restart; failing the row"
        );
        terminalize(
            &store,
            row.id,
            row.peer,
            Path::new(&row.package_ref),
            OutboundState::Failed,
            Some("preparation interrupted by a restart — send again"),
            "failed",
            None,
            None,
        );
        healed += 1;
    }
    if healed > 0 {
        tracing::info!(count = healed, "interrupted preparations healed to failed");
    }
    Ok(healed)
}
