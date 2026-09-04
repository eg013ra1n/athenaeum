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

use crate::api::sync::PrepareSource;
use crate::api::{db, ApiError};
use crate::events::{emit_event, ProgressEmitter};
use crate::export::models::CalibratedLightOptions;
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
    pub records: Vec<(PrepareSource, ManifestRecord)>,
    /// How to calibrate the [`PrepareSource::Generate`] records, when there are
    /// any. `None` for a plan that only copies.
    pub gen_opts: Option<CalibratedLightOptions>,
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

/// The row's byte total for the "still queued, nothing moving yet" signal
/// (C-1). This is the SAME number `enqueue_preparing` already wrote into
/// `sync_outbound_files` and `stage_records` (below) sums independently on
/// its own blocking thread — review fix #4: the original form of this helper
/// re-derived it by opening a fresh `CatalogSyncStore` connection and taking
/// its `std::sync::Mutex` from inside the async `tokio::spawn` body, which is
/// blocking file and lock work on an executor thread for a number the caller
/// already has in hand. Both call sites hold the plan's own records, so they
/// sum this directly instead; the helper function is gone — nothing else
/// called it.
fn preparing_bytes_total(records: &[(PrepareSource, ManifestRecord)]) -> u64 {
    records.iter().map(|(_, r)| r.byte_size).sum()
}

/// One `sync-progress { stage: "preparing", bytesDone: 0, bytesTotal }` event —
/// the "still queued, nothing moving yet" signal shared by C-1's two call
/// sites: once from `spawn_prepare` itself, before it tries for the shared
/// staging slot, and again from inside `open_generation`, before it tries for
/// the compute slot. Same shape both times, on purpose: the UI already renders
/// this as a byte fraction, and a second, differently-shaped tick would only
/// make the bar jump.
fn emit_preparing_progress(
    emitter: &dyn ProgressEmitter,
    id: i64,
    peer: NodeId,
    frame_count: u32,
    bytes_total: u64,
) {
    emit_event(
        emitter,
        "sync-progress",
        &SyncProgressEvent {
            package_id: id.to_string(),
            direction: Direction::Sent,
            stage: "preparing".to_string(),
            peer_device: node_id_hex(&peer),
            frame_count,
            project_id: None,
            bytes_done: Some(0),
            bytes_total: Some(bytes_total),
        },
    );
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
        // C-1: a preparation parked behind another one's staging slot would
        // otherwise sit silent — no bytes moving, no event, nothing in the
        // row to say why. Say so now, with the row's own known byte total,
        // before we even try for the slot.
        if let Some(em) = emitter.as_deref() {
            let bytes_total = preparing_bytes_total(&job.records);
            emit_preparing_progress(em, id, peer, job.records.len() as u32, bytes_total);
        }
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
        ctx,
        job.id,
        job.peer,
        &job.pkg_dir,
        &mut job.records,
        job.gen_opts.as_ref(),
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
/// A [`PrepareSource::Generate`] record has no file to copy: it is CALIBRATED
/// into the package here, from its source light and its linked masters, with
/// the run's [`CalibratedLightOptions`]. That work is CPU-heavy and streams
/// pixels, so the whole staging pass takes one [`ComputeQueue`] slot when any
/// record generates — and only then, because a plain copy is disk work that
/// must not queue behind a master build.
///
/// Split out of [`run_prepare`] so it can be driven synchronously, without a row
/// or an engine, by the package-shape tests.
///
/// [`ComputeQueue`]: crate::services::compute_queue::ComputeQueue
#[allow(clippy::too_many_arguments)]
pub(crate) fn stage_records(
    ctx: &ServiceContext,
    id: i64,
    peer: NodeId,
    pkg_dir: &Path,
    records: &mut [(PrepareSource, ManifestRecord)],
    gen_opts: Option<&CalibratedLightOptions>,
    emitter: Option<&Arc<dyn ProgressEmitter>>,
    flag: &Arc<AtomicBool>,
) -> Result<PrepareStats, PrepareError> {
    let cancelled = || flag.load(Ordering::SeqCst);
    // The batch total is the PLAN's estimate: a generated file's size is only
    // known once it is written, and a total that grew mid-run would make the
    // bar jump backwards. Each generated record therefore advances the bar by
    // its estimate too, so the two stay in one currency and the bar still ends
    // at 100% — the real sizes reach the manifest, the per-file rows and the
    // final stats instead.
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

    // Admission + plan resolution, in that order and BEFORE the loop — the same
    // ordering `api::export::run_export_organize` uses: taking the compute slot
    // first means the catalog connection is borrowed only for the short resolve,
    // never held across the queue wait.
    let mut generation = if records
        .iter()
        .any(|(s, _)| matches!(s, PrepareSource::Generate { .. }))
    {
        Some(open_generation(
            ctx, id, peer, records, gen_opts, emitter, flag,
        )?)
    } else {
        None
    };

    // `checked_sub`, not a plain `-`: on a fresh container the monotonic clock
    // can be younger than `PROGRESS_MIN_INTERVAL`, and subtracting past its
    // epoch panics. The `now()` fallback is NOT "tick immediately" — it sets
    // `last_tick` to right now, so the first `elapsed()` read comes back ~0
    // and the very first progress tick is DELAYED by up to one interval
    // instead of firing straight away. Acceptable only because the branch is
    // near-unreachable outside that fresh-container window (the process has
    // to be younger than `PROGRESS_MIN_INTERVAL` for this to trigger at all),
    // and even then it costs one interval's worth of a late first tick, never
    // a wrong one.
    let mut done_before: u64 = 0;
    let mut last_tick = Instant::now()
        .checked_sub(PROGRESS_MIN_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut staged_hashes: Vec<(String, u64)> = Vec::with_capacity(records.len());
    for (src, record) in records.iter() {
        let dest = pkg_dir.join(&record.rel_path);
        let size = record.byte_size;
        let rel = record.rel_path.clone();
        // Copied, not borrowed: `done_before` is fixed for the duration of this
        // file and is advanced after the closure is gone.
        let base = done_before;
        let mut file_last = Instant::now()
            .checked_sub(PROGRESS_MIN_INTERVAL)
            .unwrap_or_else(Instant::now);
        let (xxh3, real_size) = match src {
            PrepareSource::Copy(path) => {
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
                let staged = stage_payload(path, &dest, size, &cancelled, &mut on_progress)
                    .map_err(|e| {
                        if e.downcast_ref::<StageCancelled>().is_some() {
                            PrepareError::Cancelled
                        } else {
                            tracing::error!(
                                package_id = id,
                                path = %path.display(),
                                error = %format!("{e:#}"),
                                "prepare: staging a payload failed"
                            );
                            PrepareError::Failed {
                                reason: format!("{}: {e:#}", record.rel_path),
                                culprit: Some((record.rel_path.clone(), format!("{e:#}"))),
                            }
                        }
                    })?;
                // The file's terminal tick, emitted here rather than from
                // `on_progress`: a zero-byte payload reports no progress at all
                // (there is no byte to report), and a bar that never reaches its
                // own total looks stuck.
                emit_file(&record.rel_path, staged.bytes, size);
                (staged.xxh3, staged.bytes)
            }
            PrepareSource::Generate { frame_id } => {
                // Announced BEFORE the work, unlike a copy: calibrating one
                // light takes seconds to minutes with no byte-level progress to
                // report, and a file that only appears once it is finished
                // looks like a stall.
                emit_file(&rel, 0, size);
                let gen = generation
                    .as_mut()
                    .expect("a Generate record opened the generation above");
                let generated =
                    generate_payload(gen, *frame_id, &dest, flag).map_err(|e| match e {
                        PrepareError::Cancelled => PrepareError::Cancelled,
                        PrepareError::Failed { reason, .. } => {
                            tracing::error!(
                                package_id = id,
                                frame_id = *frame_id,
                                rel_path = %record.rel_path,
                                error = %reason,
                                "prepare: generating a calibrated light failed"
                            );
                            PrepareError::Failed {
                                reason: format!("{}: {reason}", record.rel_path),
                                culprit: Some((record.rel_path.clone(), reason)),
                            }
                        }
                    })?;
                // Terminal tick against the REAL size — the number this file's
                // row is about to be corrected to, so the per-file bar ends
                // full rather than at the estimate's fraction of it.
                emit_file(&record.rel_path, generated.1, generated.1);
                // And one batch tick per generated file: generation reports no
                // byte-level progress, so without this the batch bar of an
                // all-calibrated package would not move until the last frame.
                // Unthrottled on purpose — it fires once per file, and a file
                // takes seconds to minutes.
                last_tick = Instant::now();
                emit_batch(base + size);
                (generated.0, generated.1)
            }
        };
        // The estimate, not `real_size`: see the `total` comment above.
        done_before += size;
        staged_hashes.push((xxh3, real_size));
    }

    // The pixels are done: hand the compute slot back before the bookkeeping
    // below, so the next master build or export is not waiting on a manifest
    // write.
    drop(generation);

    // Correct every generated record to what actually landed: the plan could
    // only estimate it from the raw light, and the manifest is the receiver's
    // integrity contract — a wrong `byte_size` there is a rejected payload.
    let mut generated_sizes: Vec<(String, u64)> = Vec::new();
    for ((src, record), (h, real_size)) in records.iter_mut().zip(staged_hashes.into_iter()) {
        record.xxh3 = h;
        // Unconditional for a generated record, even when the estimate happened
        // to be right: one UPDATE per generated file is nothing, and a
        // "correct only when it differs" path would be one the fixtures never
        // exercise.
        if matches!(src, PrepareSource::Generate { .. }) {
            record.byte_size = real_size;
            generated_sizes.push((record.rel_path.clone(), real_size));
        }
    }
    if !generated_sizes.is_empty() {
        update_generated_file_sizes(ctx, id, &generated_sizes);
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
        // What actually landed, which is the estimate `total` only when nothing
        // was generated — the journal and the "package prepared" log should say
        // how big the package IS, not how big it was expected to be.
        bytes: manifest.iter().map(|r| r.byte_size).sum(),
    })
}

/// Write the real size of every generated payload into its `sync_outbound_files`
/// row, so the Transfers list stops showing the raw light's size for a file that
/// is now a 32-bit-float calibrated frame.
///
/// Best-effort, like the hash banking: the package on disk and its manifest are
/// the product; these rows are the UI's picture of it, and failing a prepared
/// transfer because one of them would not update would be the worse outcome.
fn update_generated_file_sizes(ctx: &ServiceContext, id: i64, sizes: &[(String, u64)]) {
    let store = match sync_store(ctx) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(package_id = id, error = %format!("{e:#}"), "prepare: cannot open store; generated sizes not corrected");
            return;
        }
    };
    for (rel_path, size) in sizes {
        if let Err(e) = store.update_outbound_file_size(id, rel_path, *size) {
            tracing::warn!(package_id = id, rel_path, error = %format!("{e:#}"), "prepare: correcting a generated file size failed");
        }
    }
}

/// Everything the staging pass needs to CALIBRATE its lights instead of copying
/// them: the compute slot it holds for the whole pass, one resolved plan per
/// generated frame, the run's options and the scratch dir the pixel phase
/// spills through.
///
/// The plans are owned (no lifetime), so the catalog connection they were
/// resolved from is dropped before the first frame is calibrated — the same
/// split [`crate::export::file_organizer::GenerationBatch`] exists for.
#[cfg(feature = "render")]
struct Generation {
    /// Held, never read: dropping it releases the compute slot.
    _permit: crate::services::compute_queue::ComputePermit,
    specs: std::collections::HashMap<i64, crate::export::GenerationSpec>,
    opts: CalibratedLightOptions,
    scratch_dir: PathBuf,
    /// One hot-pixel outcome per master dark for the whole package — measuring
    /// one costs a full plane read, and a frame set shares its dark. Refusals
    /// are cached with the maps, so a degenerate dark is measured (and logged)
    /// once per package rather than once per light.
    hot_maps: std::collections::HashMap<
        PathBuf,
        Arc<crate::calibration_library::cosmetic::HotPixelMapOutcome>,
    >,
}

/// A build with no pixel pipeline can never generate anything, so the type is
/// uninhabited there — [`stage_records`] keeps ONE body across both
/// configurations and the headless build proves its generation arm unreachable
/// (`match *gen {}`) instead of carrying dead runtime code. Mirrors
/// [`crate::export::file_organizer::GenerationBatch`].
#[cfg(not(feature = "render"))]
enum Generation {}

/// Take the compute slot, then resolve a plan for every generated record in ONE
/// short catalog borrow.
///
/// A resolve failure fails the WHOLE preparation, unlike the export — an export
/// still delivers the frames it could calibrate, but a transfer is one package
/// with one manifest, and shipping it a file short (or shipping raw bytes under
/// a `c_` name) is not an outcome the receiver could tell apart from success.
/// The culprit names the file, so the detail view says which light broke it.
#[cfg(feature = "render")]
fn open_generation(
    ctx: &ServiceContext,
    id: i64,
    peer: NodeId,
    records: &[(PrepareSource, ManifestRecord)],
    gen_opts: Option<&CalibratedLightOptions>,
    emitter: Option<&Arc<dyn ProgressEmitter>>,
    flag: &Arc<AtomicBool>,
) -> Result<Generation, PrepareError> {
    use crate::services::compute_queue::ComputeJobKind;

    let opts = gen_opts.cloned().ok_or_else(|| {
        // Unreachable through the API (the frame-set send always passes its
        // options), and deliberately NOT defaulted: calibrating with settings
        // the user did not choose would be a silent, invisible substitution.
        tracing::error!(
            package_id = id,
            "prepare: generated records with no calibration options"
        );
        PrepareError::Failed {
            reason: "calibrated-light options missing from the plan".to_string(),
            culprit: None,
        }
    })?;
    // C-1: waiting for the compute slot is its own kind of parked — one
    // already-running generation can hold it for minutes — so it gets the
    // same signal the shared staging slot does, right before the wait that
    // can make it necessary.
    tracing::info!(package_id = id, "prepare: waiting for the compute slot");
    if let Some(em) = emitter {
        let bytes_total = preparing_bytes_total(records);
        emit_preparing_progress(em.as_ref(), id, peer, records.len() as u32, bytes_total);
    }
    let _permit = match ctx.compute_queue.acquire(
        ComputeJobKind::LightCalibration,
        &format!("Transfer — calibrate lights (send {id})"),
        Arc::clone(flag),
    ) {
        Ok((permit, _job_id)) => permit,
        // `acquire` fails only when THIS flag was raised, so a cancelled ticket
        // means the preparation itself was cancelled.
        Err(_cancelled) => return Err(PrepareError::Cancelled),
    };
    let scratch_dir = std::env::temp_dir();
    let handle = db(ctx).map_err(|e| PrepareError::Failed {
        reason: format!("catalog unavailable: {e}"),
        culprit: None,
    })?;
    let conn = handle.conn();
    let mut specs = std::collections::HashMap::new();
    // ONE memo for the whole package, same reason as the export batch: the
    // flat-norm divisor can cost a full read of the master flat's plane and
    // every light in a send shares that flat. Valid for this `opts` only, which
    // is the loop's whole scope (see `export::DivisorCache`).
    let mut divisors = crate::export::DivisorCache::new();
    for (src, record) in records {
        let PrepareSource::Generate { frame_id } = src else {
            continue;
        };
        let spec = crate::export::resolve_generation_cached(
            &conn,
            *frame_id,
            &opts,
            &scratch_dir,
            &mut divisors,
        )
        .map_err(|e| {
            tracing::error!(
                package_id = id,
                frame_id = *frame_id,
                rel_path = %record.rel_path,
                error = %format!("{e:#}"),
                "prepare: cannot calibrate this light"
            );
            PrepareError::Failed {
                reason: format!("{}: {e:#}", record.rel_path),
                culprit: Some((record.rel_path.clone(), format!("{e:#}"))),
            }
        })?;
        specs.insert(*frame_id, spec);
    }
    tracing::info!(
        package_id = id,
        count = specs.len(),
        flat_divisors = divisors.len(),
        "calibrated-light generation planned for a transfer"
    );

    // C-2: stat every DISTINCT resolved master (dark/flat/bias) before a
    // single frame is generated. A batch's masters are shared across its
    // lights, so this is a handful of stats, not one per frame — and it must
    // happen here, before the loop in `stage_records` writes anything, so a
    // missing/archived master fails the WHOLE preparation up front rather
    // than partway through the package dir.
    for path in crate::export::resolved_master_paths(&specs) {
        if std::fs::metadata(&path).is_err() {
            tracing::error!(path = %path.display(), "prepare: master file missing");
            return Err(PrepareError::Failed {
                reason: format!(
                    "master file missing on disk: {} (archived or moved — restore it, then send again)",
                    path.display()
                ),
                culprit: None,
            });
        }
    }

    Ok(Generation {
        _permit,
        specs,
        opts,
        scratch_dir,
        hot_maps: std::collections::HashMap::new(),
    })
}

#[cfg(not(feature = "render"))]
fn open_generation(
    _ctx: &ServiceContext,
    _id: i64,
    _peer: NodeId,
    _records: &[(PrepareSource, ManifestRecord)],
    _gen_opts: Option<&CalibratedLightOptions>,
    _emitter: Option<&Arc<dyn ProgressEmitter>>,
    _flag: &Arc<AtomicBool>,
) -> Result<Generation, PrepareError> {
    // Unreachable: no headless producer mints a `Generate` record (composing
    // them needs `api::frame_set_send`, which is itself `render`-only). Said
    // out loud rather than silently copying the raw light under the `c_` name.
    Err(PrepareError::Failed {
        reason: "calibrated-light generation is unavailable in this build".to_string(),
        culprit: None,
    })
}

/// Calibrate one frame straight into `dest` (atomic temp + rename inside the
/// package dir). Returns its `(xxh3, byte_size)` for the manifest.
#[cfg(feature = "render")]
fn generate_payload(
    gen: &mut Generation,
    frame_id: i64,
    dest: &Path,
    flag: &AtomicBool,
) -> Result<(String, u64), PrepareError> {
    let spec = gen
        .specs
        .get(&frame_id)
        .ok_or_else(|| PrepareError::Failed {
            reason: format!("no calibration plan resolved for frame {frame_id}"),
            culprit: None,
        })?;
    let generated = crate::export::execute_generation(
        spec,
        dest,
        &gen.scratch_dir,
        &gen.opts,
        &mut gen.hot_maps,
        flag,
    )
    .map_err(|e| {
        if matches!(
            e.downcast_ref::<crate::integration::IntegrationError>(),
            Some(crate::integration::IntegrationError::Cancelled)
        ) {
            PrepareError::Cancelled
        } else {
            PrepareError::Failed {
                reason: format!("{e:#}"),
                culprit: None,
            }
        }
    })?;
    // NOT `generated.output_hash`: that one is the 3-position SAMPLING digest
    // (`duplicates::compute_xxhash`), and the manifest's contract is the
    // full-file digest the receiver re-computes with `package::xxh3_full_file`
    // before it accepts a payload. Same algorithm, different input — a sampled
    // digest would fail every verification on a file over 512 KB.
    let xxh3 = crate::package::xxh3_full_file(dest).map_err(|e| PrepareError::Failed {
        reason: format!("hash generated payload: {e:#}"),
        culprit: None,
    })?;
    // A send has no per-file warning channel (its failure policy is
    // all-or-nothing), so a non-fatal note — today only a refused hot-pixel map,
    // raised once per master dark — is logged against the frame that met it.
    for note in &generated.warnings {
        tracing::warn!(frame_id, note = %note, "calibrated light staged with a warning");
    }
    tracing::debug!(
        frame_id,
        dest = %dest.display(),
        calstat = %generated.calstat,
        debayered = generated.debayered,
        hot_pixels_replaced = generated.hot_pixels_replaced,
        bytes = generated.byte_size,
        "calibrated light staged into a package"
    );
    Ok((xxh3, generated.byte_size))
}

#[cfg(not(feature = "render"))]
fn generate_payload(
    gen: &mut Generation,
    _frame_id: i64,
    _dest: &Path,
    _flag: &AtomicBool,
) -> Result<(String, u64), PrepareError> {
    match *gen {}
}

/// Write the full hashes the staging pass computed into `files.strong_hash`, for
/// every candidate the disk still vouches for. Best-effort by design: the
/// package is the product, the banked hash a by-product.
///
/// Generated payloads never appear here: the plan banks no candidate for them
/// (their catalog row describes the source light, not the artifact).
pub(crate) fn bank_prepared_hashes(
    conn: &rusqlite::Connection,
    records: &[(PrepareSource, ManifestRecord)],
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

// The generation seam needs the pixel pipeline, so the fixture rides the same
// gate as the code under test.
#[cfg(all(test, feature = "render"))]
mod tests {
    use super::*;
    use crate::export::models::CalibratedLightOptions;
    use crate::fits_writer::write_fits_f32;
    use crate::package::{ManifestRecord, PayloadKind, MANIFEST_VERSION};
    use rusqlite::{params, Connection};

    const W: usize = 16;
    const H: usize = 16;

    fn write_plane(path: &Path, fill: impl Fn(usize, usize) -> f32) {
        let mut data = vec![0f32; W * H];
        for y in 0..H {
            for x in 0..W {
                data[y * W + x] = fill(x, y);
            }
        }
        write_fits_f32(path, W, H, 1, &data, &[]).unwrap();
    }

    /// A catalog holding one LIGHT (frame 10) linked to a built master dark, and
    /// the light's path. The dark alternates 300/302 with two spikes so the
    /// cosmetic pass has a real map to build — a flat dark yields none (zero
    /// MAD), which would leave that stage untested.
    fn seed(ctx: &ServiceContext, dir: &Path) -> PathBuf {
        let light = dir.join("L_10.fits");
        let dark = dir.join("master_dark.fits");
        write_plane(&light, |_, _| 1000.0);
        write_plane(&dark, |x, y| {
            if (x, y) == (5, 5) || (x, y) == (9, 9) {
                5000.0
            } else if (x + y) % 2 == 0 {
                300.0
            } else {
                302.0
            }
        });
        let handle = db(ctx).unwrap();
        let conn: &Connection = &handle.conn();
        let insert_file = |id: i64, path: &Path| {
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
                params![
                    id,
                    path.to_string_lossy(),
                    path.file_name().unwrap().to_string_lossy()
                ],
            )
            .unwrap();
        };
        insert_file(1, &light);
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, instrume, object, date_obs, uuid)
             VALUES (10, 1, 'Light', 'TestCam', 'M31', '2026-07-05T20:30:00Z', 'uuid-10')",
            [],
        )
        .unwrap();
        insert_file(2, &dark);
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, is_master) VALUES (20, 2, 'MasterDark', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (100, 'Dark', '2026-07-05', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (100, 20)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (10, 'frame', 100, 'Dark', '2026-07-05T00:00:00Z')",
            [],
        )
        .unwrap();
        light
    }

    /// One `Generate` record, planned exactly as `plan_selection_package` would:
    /// the estimate is the RAW light's size, and the hash is still empty.
    fn generate_record(rel_path: &str, estimate: u64) -> (PrepareSource, ManifestRecord) {
        (
            PrepareSource::Generate { frame_id: 10 },
            ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: "artifact-uuid".to_string(),
                origin_catalog_uuid: "artifact-uuid".to_string(),
                origin_device: "ab".repeat(32),
                payload_kind: PayloadKind::CalibratedLight,
                rel_path: rel_path.to_string(),
                byte_size: estimate,
                xxh3: String::new(),
                frame_meta: serde_json::json!({}),
                analysis: None,
                app_version: "test".to_string(),
                project: None,
            },
        )
    }

    /// A second light (frame 11, file id 3) sharing `seed()`'s master dark —
    /// used only by the mid-generation cancel test, which cancels BETWEEN two
    /// lights' generations rather than during either one's own (tiny,
    /// single-band) pixel loop.
    fn seed_second_light(ctx: &ServiceContext, dir: &Path) -> PathBuf {
        let light = dir.join("L_11.fits");
        write_plane(&light, |_, _| 1000.0);
        let handle = db(ctx).unwrap();
        let conn: &Connection = &handle.conn();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (3, ?1, ?2, 0, '2026-07-05T00:00:00Z', 'FITS')",
            params![
                light.to_string_lossy(),
                light.file_name().unwrap().to_string_lossy()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, instrume, object, date_obs, uuid)
             VALUES (11, 3, 'Light', 'TestCam', 'M31', '2026-07-05T20:31:00Z', 'uuid-11')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (11, 'frame', 100, 'Dark', '2026-07-05T00:00:00Z')",
            [],
        )
        .unwrap();
        light
    }

    /// [`generate_record`] with an explicit `frame_id`, for a batch of more
    /// than one generated light.
    fn generate_record_for(
        frame_id: i64,
        rel_path: &str,
        estimate: u64,
    ) -> (PrepareSource, ManifestRecord) {
        (
            PrepareSource::Generate { frame_id },
            ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: format!("artifact-uuid-{frame_id}"),
                origin_catalog_uuid: format!("artifact-uuid-{frame_id}"),
                origin_device: "ab".repeat(32),
                payload_kind: PayloadKind::CalibratedLight,
                rel_path: rel_path.to_string(),
                byte_size: estimate,
                xxh3: String::new(),
                frame_meta: serde_json::json!({}),
                analysis: None,
                app_version: "test".to_string(),
                project: None,
            },
        )
    }

    /// A `Copy` record for a real on-disk file — the C-1 signal test does not
    /// need the calibration engine at all, only a preparation with SOMETHING
    /// to stage once the shared slot is free.
    fn copy_record(rel_path: &str, path: &Path, byte_size: u64) -> (PrepareSource, ManifestRecord) {
        (
            PrepareSource::Copy(path.to_path_buf()),
            ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: "uuid-1".to_string(),
                origin_catalog_uuid: "uuid-1".to_string(),
                origin_device: "ab".repeat(32),
                payload_kind: PayloadKind::RawFrame,
                rel_path: rel_path.to_string(),
                byte_size,
                xxh3: String::new(),
                frame_meta: serde_json::json!({}),
                analysis: None,
                app_version: "test".to_string(),
                project: None,
            },
        )
    }

    /// A working `SyncEngineHandle` bound to `ctx`'s own sync store, over a
    /// fresh in-process loopback transport, addressed at a NEVER-STARTED peer.
    /// Enough for `PrepareJob.engine` to exist for a `spawn_prepare` test;
    /// neither test that uses it drives a package to completion (both assert
    /// on the row before that could happen), so a `drive` reaching for an
    /// unstarted peer is harmless — logged, never awaited on.
    fn test_engine(ctx: &ServiceContext) -> (Arc<crate::sync::engine::SyncEngineHandle>, NodeId) {
        let store: Arc<dyn SyncStore> = Arc::new(sync_store(ctx).unwrap());
        let net = crate::sharing::loopback::LoopbackNetwork::new();
        let sender_transport = Arc::new(net.endpoint());
        let dest_peer = net.endpoint().node_id();
        (
            Arc::new(crate::sync::engine::SyncEngine::spawn(
                store,
                sender_transport,
                dest_peer,
            )),
            dest_peer,
        )
    }

    /// Records every emitted `(event_name, payload)` — mirrors
    /// `sync::engine_tests::CapturingEmitter`, private to that file.
    #[derive(Default)]
    struct CapturingEmitter(std::sync::Mutex<Vec<(String, serde_json::Value)>>);
    impl crate::events::ProgressEmitter for CapturingEmitter {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            self.0
                .lock()
                .unwrap()
                .push((event_name.to_string(), payload));
        }
    }
    impl CapturingEmitter {
        fn named(&self, name: &str) -> Vec<serde_json::Value> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, p)| p.clone())
                .collect()
        }
    }

    /// Sets the preparation's own cancel flag the moment it sees the SECOND
    /// light's "about to generate" tick (`sync-file-progress`, `bytesDone ==
    /// 0`) — the same reacting-emitter shape `api::sync::CancelWhenStaged`
    /// uses for the copy path, aimed one step earlier: frame 1 finishes
    /// generating uncancelled, and frame 2's very first (only, for this
    /// 16x16 fixture) band-loop check inside the engine observes the flag
    /// already raised — genuinely mid-generation, not at admission.
    struct CancelBeforeSecondGenerate {
        sender: Arc<SyncSenderRuntime>,
        id: i64,
        seen: std::sync::atomic::AtomicUsize,
    }
    impl crate::events::ProgressEmitter for CancelBeforeSecondGenerate {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            if event_name != "sync-file-progress" {
                return;
            }
            if payload.get("bytesDone").and_then(|v| v.as_u64()) != Some(0) {
                return;
            }
            if self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == 2 {
                self.sender.prepare().cancel(self.id);
            }
        }
    }

    /// The whole point of the task: a `Generate` record puts CALIBRATED bytes in
    /// the package under the `c_` name — not a copy of the raw light — and the
    /// manifest describes what actually landed (full-file hash + real size), so
    /// the receiver's own verification accepts it. The transfer's own per-file
    /// row is corrected to the same real size.
    #[test]
    fn stage_records_generates_into_package() {
        use crate::sharing::types::{AnnounceFileEntry, PackageLayout};

        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let light = seed(&ctx, tmp.path());
        let estimate = std::fs::metadata(&light).unwrap().len();

        let rel = "camera_testcam/lights/c_L_10.fits";
        let pkg_dir = tmp.path().join("packages").join("pkg-1");
        // A real `preparing` row with its per-file row, exactly as the enqueue
        // writes one — the size correction has somewhere to land.
        let store = sync_store(&ctx).unwrap();
        let id = store
            .enqueue_preparing(
                &pkg_dir.to_string_lossy(),
                [0u8; 32],
                Some("M31 calibrated"),
                &[AnnounceFileEntry {
                    rel_path: rel.to_string(),
                    byte_size: estimate,
                    frame_uuid: "artifact-uuid".to_string(),
                }],
                PackageLayout::Batch,
            )
            .unwrap();

        let mut records = vec![generate_record(rel, estimate)];
        let flag = Arc::new(AtomicBool::new(false));
        let stats = stage_records(
            &ctx,
            id,
            [0u8; 32],
            &pkg_dir,
            &mut records,
            Some(&CalibratedLightOptions::default()),
            None,
            &flag,
        )
        .unwrap_or_else(|e| panic!("staging failed: {}", e.describe()));

        let dest = pkg_dir.join(rel);
        assert!(dest.exists(), "the generated file landed in the package");
        let real_size = std::fs::metadata(&dest).unwrap().len();
        let full_hash = crate::package::xxh3_full_file(&dest).unwrap();

        // The manifest is the receiver's contract: it must describe the file on
        // disk, not the raw light the plan estimated from.
        let manifest = crate::package::read_manifest(&pkg_dir).unwrap();
        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest[0].xxh3, full_hash);
        assert!(!manifest[0].xxh3.is_empty());
        assert_eq!(manifest[0].byte_size, real_size);
        assert_eq!(records[0].1.byte_size, real_size);
        assert_eq!(stats.files, 1);
        assert_eq!(stats.bytes, real_size, "stats report what landed");

        // …and so does the row the Transfers list reads. That the UPDATE itself
        // writes what it is told is pinned by
        // `store::tests::update_outbound_file_size_corrects_one_row`; here the
        // point is that the two halves agree after a real staging pass.
        let rows = crate::sync::store::list_outbound_files(&store.lock_conn(), id).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].byte_size, real_size, "per-file row corrected");

        // And the bytes are the CALIBRATED frame, not the source: (1000 − ~300)
        // per pixel, so the two files cannot hash alike.
        assert_ne!(
            full_hash,
            crate::package::xxh3_full_file(&light).unwrap(),
            "the package must not hold a copy of the raw light"
        );
    }

    /// A cancel raised before staging stops the generation at admission — the
    /// compute queue hands back a cancelled ticket, which is the preparation's
    /// own flag, and nothing is written.
    #[test]
    fn stage_records_generation_honours_a_cancel() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let light = seed(&ctx, tmp.path());
        let estimate = std::fs::metadata(&light).unwrap().len();

        let rel = "camera_testcam/lights/c_L_10.fits";
        let mut records = vec![generate_record(rel, estimate)];
        let pkg_dir = tmp.path().join("packages").join("pkg-2");
        let flag = Arc::new(AtomicBool::new(true));
        let err = stage_records(
            &ctx,
            0,
            [0u8; 32],
            &pkg_dir,
            &mut records,
            Some(&CalibratedLightOptions::default()),
            None,
            &flag,
        )
        .err()
        .expect("a cancelled preparation does not stage");
        assert!(matches!(err, PrepareError::Cancelled), "{}", err.describe());
        assert!(!pkg_dir.join(rel).exists());
    }

    /// C-2: a resolved master whose FILE was deleted (archived or moved — the
    /// catalog row is untouched) fails the WHOLE preparation early, before a
    /// single output file lands in the package dir. Reuses `seed()`'s fixture
    /// and removes its master dark from disk.
    #[test]
    fn stage_records_fails_early_on_a_missing_master_file() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let light = seed(&ctx, tmp.path());
        let estimate = std::fs::metadata(&light).unwrap().len();
        std::fs::remove_file(tmp.path().join("master_dark.fits")).unwrap();

        let rel = "camera_testcam/lights/c_L_10.fits";
        let mut records = vec![generate_record(rel, estimate)];
        let pkg_dir = tmp.path().join("packages").join("pkg-missing-master");
        let flag = Arc::new(AtomicBool::new(false));
        let err = stage_records(
            &ctx,
            0,
            [0u8; 32],
            &pkg_dir,
            &mut records,
            Some(&CalibratedLightOptions::default()),
            None,
            &flag,
        )
        .err()
        .expect("a missing master must fail the whole preparation");
        assert!(
            matches!(err, PrepareError::Failed { .. }),
            "{}",
            err.describe()
        );
        assert!(
            err.describe().contains("master file missing"),
            "{}",
            err.describe()
        );
        assert!(
            !pkg_dir.join(rel).exists(),
            "nothing partial lands when a master is missing"
        );
    }

    /// A plan that generates without options is refused rather than calibrated
    /// with defaults the user never chose. Unreachable through the API; the
    /// failure text is what a developer would see if it ever became reachable.
    #[test]
    fn stage_records_refuses_to_generate_without_options() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let light = seed(&ctx, tmp.path());
        let estimate = std::fs::metadata(&light).unwrap().len();

        let mut records = vec![generate_record("lights/c_L_10.fits", estimate)];
        let pkg_dir = tmp.path().join("packages").join("pkg-3");
        let flag = Arc::new(AtomicBool::new(false));
        let err = stage_records(
            &ctx,
            0,
            [0u8; 32],
            &pkg_dir,
            &mut records,
            None,
            None,
            &flag,
        )
        .err()
        .expect("no options, no generation");
        assert!(
            err.describe().contains("options missing"),
            "{}",
            err.describe()
        );
    }

    /// C-1 items 1 and 4: a preparation that has to wait for the shared
    /// staging slot is not silent while it waits — it announces the row's own
    /// known byte total before it ever tries for the slot, so a second send
    /// queued behind another one shows a byte fraction instead of nothing at
    /// all. The test holds the slot itself, so any event `spawn_prepare`
    /// emits before releasing it necessarily happened before the wait.
    #[tokio::test]
    async fn spawn_prepare_emits_preparing_signal_before_the_slot() {
        use crate::sharing::types::{AnnounceFileEntry, PackageLayout};

        let tmp = tempfile::tempdir().unwrap();
        let ctx = Arc::new(ServiceContext::new_for_tests(tmp.path().join("catalog.db")));

        let src = tmp.path().join("L_1.fits");
        std::fs::write(&src, vec![7u8; 4096]).unwrap();
        let rel = "camera_testcam/lights/L_1.fits";
        let pkg_dir = tmp.path().join("packages").join("pkg-parked");

        let (engine, dest_peer) = test_engine(&ctx);
        let store = sync_store(&ctx).unwrap();
        let id = store
            .enqueue_preparing(
                &pkg_dir.to_string_lossy(),
                dest_peer,
                Some("M31"),
                &[AnnounceFileEntry {
                    rel_path: rel.to_string(),
                    byte_size: 4096,
                    frame_uuid: "uuid-1".to_string(),
                }],
                PackageLayout::Batch,
            )
            .unwrap();

        let emitter = Arc::new(CapturingEmitter::default());
        let sender = Arc::new(SyncSenderRuntime::new());
        // Hold the ONE staging slot ourselves: `spawn_prepare`'s task must
        // wait for it, so anything it emits from THIS test happened before
        // that wait, not after.
        let held = sender.prepare().slot().acquire_owned().await.unwrap();

        let job = PrepareJob {
            id,
            peer: dest_peer,
            pkg_dir: pkg_dir.clone(),
            records: vec![copy_record(rel, &src, 4096)],
            gen_opts: None,
            bank: Vec::new(),
            engine,
            emitter: Some(Arc::clone(&emitter) as Arc<dyn ProgressEmitter>),
        };
        spawn_prepare(Arc::clone(&ctx), Arc::clone(&sender), job);

        let deadline = Instant::now() + Duration::from_secs(5);
        let tick = loop {
            if let Some(t) = emitter
                .named("sync-progress")
                .into_iter()
                .find(|e| e["stage"] == "preparing")
            {
                break t;
            }
            assert!(
                Instant::now() < deadline,
                "no preparing signal arrived while parked behind the slot"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(tick["bytesDone"], serde_json::json!(0));
        assert_eq!(tick["bytesTotal"], serde_json::json!(4096));

        drop(held);
    }

    /// Sync Minor: a cancel raised WHILE generation is under way (between two
    /// lights in the same batch — not before admission, which
    /// `stage_records_generation_honours_a_cancel` already covers) maps to
    /// `PrepareError::Cancelled`, and the ROW it belongs to settles
    /// `cancelled` — never `failed`, which is what a mismapped
    /// `IntegrationError::Cancelled` would produce.
    #[tokio::test]
    async fn mid_generation_cancel_ends_the_row_cancelled_not_failed() {
        use crate::sharing::types::{AnnounceFileEntry, PackageLayout};

        let tmp = tempfile::tempdir().unwrap();
        let ctx = Arc::new(ServiceContext::new_for_tests(tmp.path().join("catalog.db")));
        let light1 = seed(&ctx, tmp.path());
        let light2 = seed_second_light(&ctx, tmp.path());
        let est1 = std::fs::metadata(&light1).unwrap().len();
        let est2 = std::fs::metadata(&light2).unwrap().len();

        let rel1 = "camera_testcam/lights/c_L_10.fits";
        let rel2 = "camera_testcam/lights/c_L_11.fits";
        let pkg_dir = tmp.path().join("packages").join("pkg-mid-cancel");

        let (engine, dest_peer) = test_engine(&ctx);
        let store = sync_store(&ctx).unwrap();
        let id = store
            .enqueue_preparing(
                &pkg_dir.to_string_lossy(),
                dest_peer,
                Some("M31 calibrated"),
                &[
                    AnnounceFileEntry {
                        rel_path: rel1.to_string(),
                        byte_size: est1,
                        frame_uuid: "artifact-uuid-10".to_string(),
                    },
                    AnnounceFileEntry {
                        rel_path: rel2.to_string(),
                        byte_size: est2,
                        frame_uuid: "artifact-uuid-11".to_string(),
                    },
                ],
                PackageLayout::Batch,
            )
            .unwrap();

        let sender = Arc::new(SyncSenderRuntime::new());
        let emitter = Arc::new(CancelBeforeSecondGenerate {
            sender: Arc::clone(&sender),
            id,
            seen: Default::default(),
        });

        let job = PrepareJob {
            id,
            peer: dest_peer,
            pkg_dir: pkg_dir.clone(),
            records: vec![
                generate_record_for(10, rel1, est1),
                generate_record_for(11, rel2, est2),
            ],
            gen_opts: Some(CalibratedLightOptions::default()),
            bank: Vec::new(),
            engine,
            emitter: Some(Arc::clone(&emitter) as Arc<dyn ProgressEmitter>),
        };
        spawn_prepare(Arc::clone(&ctx), Arc::clone(&sender), job);

        let deadline = Instant::now() + Duration::from_secs(10);
        let row = loop {
            let row = sync_store(&ctx).unwrap().get_outbound(id).unwrap().unwrap();
            if row.state == OutboundState::Cancelled || row.state == OutboundState::Failed {
                break row;
            }
            assert!(Instant::now() < deadline, "preparation did not terminate");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(
            row.state,
            OutboundState::Cancelled,
            "a mid-generation cancel is not a failure"
        );
        assert!(!pkg_dir.exists(), "the partial package dir is removed");
    }
}
