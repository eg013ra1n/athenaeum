//! Operator resend surfaces for the web console (Transfers Batch Model v2.1):
//!
//! - [`resend_in_place`] — the sanctioned retry: reset the SAME outbound row
//!   (`generation`+1, fresh per-attempt wire id) and re-drive it on its own
//!   peer's engine. Works for `failed`, `cancelled` (sender-side), and —
//!   because [`rebuild_package_payloads`] can resurrect a cleaned package dir
//!   from the original capture files — `confirmed` rows ("Send again" after
//!   the receiver deleted its copies). Same `batch_uuid` ⇒ the receiver's
//!   existing inbound row resets in place; its dedup handshake re-ingests only
//!   what is actually missing on that side.
//! - [`send_batch_to_target`] — mint a NEW transfer from an already-recorded
//!   batch, to any peer with a running engine: fresh dir basename ⇒ fresh wire
//!   `batch_uuid` ⇒ a brand-new inbound row on that peer. Its payload comes
//!   from the package dir when it still holds one, else from the original
//!   capture files (eligible subset: what is gone is dropped and counted, only
//!   an all-gone batch is refused). [`SourceDisposition`] decides what happens
//!   to the source identity.
//! - [`resend_declined_as_new`] — the deliberate operator re-ask for a
//!   **receiver-declined** transfer, i.e. [`send_batch_to_target`] back at the
//!   row's own peer with the declined identity retired. A decline is final per
//!   `batch_uuid` (same-batch re-announces bounce all-cancelled, and Perseus's
//!   autonomous retries must never override a human decline), so the new
//!   `batch_uuid` is what gets the receiver a fresh inbound row, while the
//!   declined row stays as history.
//!
//! Mirrors the desktop's `api::sync::resend_transfer` /
//! `resend_declined_as_new_transfer` semantics, rebuilt on Perseus's own
//! bookkeeping (`perseus_seen` / `perseus_batch_files` instead of
//! `sync_sources` — the desktop implementation is welded to `ServiceContext`
//! and not extractable; the shared core primitives ARE reused:
//! `reset_outbound_for_resend`, `CANCELLED_BY_RECEIVER_DETAIL`,
//! `read_manifest` / `xxh3_full_file`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use athenaeum_core::package::{self, read_manifest, ManifestRecord, MANIFEST_FILENAME};
use athenaeum_core::sharing::types::{AnnounceFileEntry, PackageLayout};
use athenaeum_core::sync::store::{StandaloneSyncStore, SyncStore};
use athenaeum_core::sync::{
    OutboundFileRow, OutboundFileState, OutboundRow, OutboundState, SharedPackageCleanup,
    SyncEngineHandle, CANCELLED_BY_RECEIVER_DETAIL,
};

use crate::batch_store::BatchStore;
use crate::config::Config;
use crate::seen::SeenStore;

/// True iff `row` is a transfer the RECEIVER declined — `Cancelled` with the
/// exact all-cancelled-ack detail. The strict equality is deliberate: after a
/// successful divert the old row's error gains a `— resent as new transfer #N`
/// suffix, which breaks this equality (a second divert of the same row is
/// rejected) while the UI's "declined by receiver" rendering survives by
/// prefix.
pub(crate) fn is_declined(row: &OutboundRow) -> bool {
    row.state == OutboundState::Cancelled
        && row.last_error.as_deref() == Some(CANCELLED_BY_RECEIVER_DETAIL)
}

/// True iff `dir` holds `manifest.ndjson` AND at least one non-manifest regular
/// file — i.e. there is real payload left to re-serve. A confirmed-then-cleaned
/// dir is manifest-only and a vanished dir fails `read_dir`; both return
/// `false`, which is the resend paths' cue to rebuild from the originals.
pub(crate) fn package_has_payload(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut has_manifest = false;
    let mut has_payload = false;
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            // A manifest rel_path may carry a subdirectory; treat any dir as
            // (potential) payload — cleanup removes emptied subdirs, so one
            // only exists while payload does.
            has_payload = true;
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        if entry.file_name() == std::ffi::OsStr::new(MANIFEST_FILENAME) {
            has_manifest = true;
        } else {
            has_payload = true;
        }
    }
    has_manifest && has_payload
}

/// What one [`rebuild_package_payloads`] pass did, in manifest `rel_path`s:
/// `restored` files are on disk in the package dir again (freshly copied or
/// already present), `missing` originals could not be found anywhere, and
/// `changed` originals were found but no longer match the manifest's
/// `(byte_size, xxh3)` — a changed capture file is a NEW snapshot (the watcher
/// re-enqueues it on its own), so it is honestly excluded rather than silently
/// substituted into a batch that promised different bytes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RebuildReport {
    pub restored: Vec<String>,
    pub missing: Vec<String>,
    pub changed: Vec<String>,
}

impl RebuildReport {
    /// A rebuild that dropped nothing (every manifest record is on disk again).
    pub fn is_full(&self) -> bool {
        self.missing.is_empty() && self.changed.is_empty()
    }
}

/// Where a manifest record's payload bytes will come from.
enum RestoreSource {
    /// A valid payload copy already sits in the planned package dir at this
    /// path. Restoring in place is a no-op (double-click / crash-resume
    /// idempotency — it must never be "copied onto itself"); materializing a
    /// SECOND package dir from the same manifest copies it across.
    Present(PathBuf),
    /// Verified original capture file at this path.
    Original(PathBuf),
}

impl RestoreSource {
    fn path(&self) -> &Path {
        match self {
            RestoreSource::Present(p) | RestoreSource::Original(p) => p,
        }
    }
}

/// A resolved but not-yet-executed rebuild: where every manifest record's bytes
/// would come from, and what would be dropped.
///
/// Building one is READ-ONLY (it stats and hashes candidates, and writes
/// nothing), which is what lets the same resolution serve two callers: the §6
/// confirm's dry-run — "sends 97 of 100 (3 deleted locally)" — and the first
/// half of [`rebuild_package_payloads`]. [`apply`](Self::apply) then
/// materializes it into a package dir, which may be the one it was planned
/// against (an in-place rebuild) or a fresh one (a divert that must leave the
/// source batch untouched).
pub struct RebuildPlan {
    /// The dir the manifest was read from — also where a `Present` source lives.
    src_dir: PathBuf,
    /// Every restorable record with its resolved byte source, in manifest order.
    /// Records that resolved to nothing are absent; they are named in
    /// `report.missing` / `report.changed`.
    restorable: Vec<(ManifestRecord, RestoreSource)>,
    /// How many records the manifest carried before resolution.
    total: usize,
    /// The honesty report (`restored` / `missing` / `changed` rel_paths).
    pub report: RebuildReport,
}

impl RebuildPlan {
    /// Manifest records the batch was recorded with.
    pub fn total(&self) -> usize {
        self.total
    }

    /// Records that can still be served (the eligible subset).
    pub fn sendable(&self) -> usize {
        self.restorable.len()
    }

    /// Records that would be dropped — original gone, or found but rewritten
    /// since (a changed capture file is a NEW snapshot, never this batch's).
    pub fn skipped(&self) -> usize {
        self.report.missing.len() + self.report.changed.len()
    }

    /// Materialize the plan into `dest_dir`: copy every restorable record's
    /// bytes to its `rel_path` there and write the manifest that describes
    /// exactly what landed.
    ///
    /// The manifest is (re)written whenever it is not already exactly the set
    /// just materialized: a fresh `dest_dir` has none at all, and an in-place
    /// partial rebuild must shrink to the restored subset so the package never
    /// advertises files it cannot serve.
    pub fn apply(&self, dest_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dest_dir)
            .with_context(|| format!("create package dir {}", dest_dir.display()))?;
        for (record, source) in &self.restorable {
            let dest = dest_dir.join(&record.rel_path);
            let src = source.path();
            if src == dest {
                continue; // already in place
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create payload dir {}", parent.display()))?;
            }
            let copied = std::fs::copy(src, &dest)
                .with_context(|| format!("restore {} -> {}", src.display(), dest.display()))?;
            // Same integrity guard as the package writer: the copy must satisfy
            // the manifest it will be served under.
            if copied != record.byte_size {
                bail!(
                    "restored copy size mismatch for {}: copied {} bytes, manifest byte_size {}",
                    record.rel_path,
                    copied,
                    record.byte_size
                );
            }
        }
        if dest_dir != self.src_dir || !self.report.is_full() {
            let records: Vec<&ManifestRecord> =
                self.restorable.iter().map(|(record, _)| record).collect();
            write_manifest_atomic(dest_dir, &records)?;
        }
        Ok(())
    }
}

/// Resolve — without touching anything — where each of `pkg_dir`'s manifest
/// records would get its payload bytes, using the surviving `manifest.ndjson`
/// as the authority (it is the one file payload cleanup keeps). Source
/// resolution per record, in order:
///
/// 1. a valid payload copy already in the package dir;
/// 2. the durable `perseus_batch_files` linkage (`rel_path → source_path`);
/// 3. reverse-mapping the `rel_path` onto every configured capture dir — with
///    and without the multi-dir label prefix (pre-linkage batches).
///
/// Every candidate is verified against the record's `(byte_size, xxh3)` before
/// it is accepted, so a lossy label or a moved file can never smuggle wrong
/// bytes into the batch; the ORIGINAL manifest records (and thus `frame_uuid`s)
/// are kept byte-for-byte — the receiver's dedup then skips what it still has
/// and ingests what it deleted.
///
/// A zero-restorable plan is returned, not refused: the caller decides whether
/// that is an error (a rebuild) or an answer (the dry-run behind a confirm).
pub fn plan_package_rebuild(
    pkg_dir: &Path,
    config: &Config,
    batches: &BatchStore,
) -> Result<RebuildPlan> {
    let records = read_manifest(pkg_dir)
        .with_context(|| format!("read manifest for rebuild {}", pkg_dir.display()))?;
    if records.is_empty() {
        bail!("package manifest is empty; nothing to rebuild");
    }

    let linkage: HashMap<String, PathBuf> = batches
        .files_for(&pkg_dir.to_string_lossy())
        .unwrap_or_else(|error| {
            tracing::warn!(%error, dir = %pkg_dir.display(), "batch file linkage read failed; falling back to reverse-mapping");
            Vec::new()
        })
        .into_iter()
        .collect();
    let capture_dirs = config.capture_dirs_resolved();

    let total = records.len();
    let mut report = RebuildReport::default();
    let mut restorable: Vec<(ManifestRecord, RestoreSource)> = Vec::new();
    for record in records {
        // Idempotency: a payload copy that already matches the manifest needs
        // no work when the plan is applied in place.
        let dest = pkg_dir.join(&record.rel_path);
        if verify_source(&dest, &record).is_some() {
            report.restored.push(record.rel_path.clone());
            restorable.push((record, RestoreSource::Present(dest)));
            continue;
        }

        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(src) = linkage.get(&record.rel_path) {
            candidates.push(src.clone());
        }
        for dir in &capture_dirs {
            // Single-dir batches: rel_path is capture-dir-relative as-is.
            candidates.push(dir.join(&record.rel_path));
            // Multi-dir batches: the first segment is the sanitized dir label
            // (see `run::compute_rel_path`) — strip it for the dir it names.
            if let Some((label, rest)) = record.rel_path.split_once('/') {
                if crate::run::sanitize_label(dir) == label {
                    candidates.push(dir.join(rest));
                }
            }
        }

        match resolve_source(&candidates, &record) {
            SourceResolution::Verified(src) => {
                report.restored.push(record.rel_path.clone());
                restorable.push((record, RestoreSource::Original(src)));
            }
            SourceResolution::Changed => report.changed.push(record.rel_path),
            SourceResolution::Missing => report.missing.push(record.rel_path),
        }
    }

    Ok(RebuildPlan {
        src_dir: pkg_dir.to_path_buf(),
        restorable,
        total,
        report,
    })
}

/// Restore a package dir's payload copies from the original capture files, in
/// place: [`plan_package_rebuild`] + [`RebuildPlan::apply`] onto the same dir.
///
/// A partial restore rewrites the manifest to the restored subset (the batch
/// honestly shrinks); zero restorable files is an error raised BEFORE any
/// mutation.
pub fn rebuild_package_payloads(
    pkg_dir: &Path,
    config: &Config,
    batches: &BatchStore,
) -> Result<RebuildReport> {
    let plan = plan_package_rebuild(pkg_dir, config, batches)?;
    if plan.sendable() == 0 {
        bail!(
            "none of the {} original files could be restored ({} missing, {} changed) — nothing to resend",
            plan.total(),
            plan.report.missing.len(),
            plan.report.changed.len()
        );
    }
    plan.apply(pkg_dir)?;

    tracing::info!(
        dir = %pkg_dir.display(),
        restored = plan.report.restored.len(),
        missing = plan.report.missing.len(),
        changed = plan.report.changed.len(),
        "package payloads rebuilt from originals"
    );
    Ok(plan.report)
}

/// How a record's source candidates resolved: a verified path, "found but the
/// bytes changed", or nothing on disk at all.
enum SourceResolution {
    Verified(PathBuf),
    Changed,
    Missing,
}

fn resolve_source(candidates: &[PathBuf], record: &ManifestRecord) -> SourceResolution {
    let mut saw_changed = false;
    for candidate in candidates {
        if !candidate.is_file() {
            continue;
        }
        if verify_source(candidate, record).is_some() {
            return SourceResolution::Verified(candidate.clone());
        }
        saw_changed = true;
    }
    if saw_changed {
        SourceResolution::Changed
    } else {
        SourceResolution::Missing
    }
}

/// `Some(())` iff the file at `path` matches the record's `(byte_size, xxh3)`.
/// Size is checked first so an obviously-different file skips the full hash.
fn verify_source(path: &Path, record: &ManifestRecord) -> Option<()> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() != record.byte_size {
        return None;
    }
    match package::xxh3_full_file(path) {
        Ok(hash) if hash == record.xxh3 => Some(()),
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "hash candidate for rebuild failed; skipping");
            None
        }
    }
}

/// Rewrite `manifest.ndjson` to exactly `records` (writer-identical NDJSON:
/// one compact JSON object per line), via tmp + atomic rename so a crash never
/// leaves a torn manifest — it is the one file that must survive everything.
fn write_manifest_atomic(pkg_dir: &Path, records: &[&ManifestRecord]) -> Result<()> {
    let mut buf = String::new();
    for record in records {
        let line = serde_json::to_string(record).context("serialize manifest record")?;
        buf.push_str(&line);
        buf.push('\n');
    }
    let tmp = pkg_dir.join(".manifest.ndjson.tmp");
    std::fs::write(&tmp, buf.as_bytes())
        .with_context(|| format!("write manifest tmp {}", tmp.display()))?;
    let manifest = pkg_dir.join(MANIFEST_FILENAME);
    std::fs::rename(&tmp, &manifest)
        .with_context(|| format!("replace manifest {}", manifest.display()))?;
    Ok(())
}

/// Fresh `pending` per-file rows for `records`, mirroring what the engine's
/// enqueue writes — used after a partial rebuild so `sync_outbound_files`
/// matches the rewritten manifest (the announce is built from these rows).
fn pending_file_rows(outbound_id: i64, records: &[ManifestRecord]) -> Vec<OutboundFileRow> {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    records
        .iter()
        .map(|record| OutboundFileRow {
            outbound_id,
            rel_path: record.rel_path.clone(),
            byte_size: record.byte_size,
            frame_uuid: record.frame_uuid.clone(),
            state: OutboundFileState::Pending,
            bytes_done: 0,
            outcome: None,
            error: None,
            updated_at: now.clone(),
        })
        .collect()
}

/// The result of one accepted [`resend_in_place`]: the row id (unchanged —
/// v2.1 resets in place, never mints rows) plus the rebuild honesty report
/// (`missing` / `changed` rel_paths, empty when the payload was still on disk).
#[derive(Debug, Default)]
pub struct ResendReport {
    pub rebuilt: bool,
    pub missing: Vec<String>,
    pub changed: Vec<String>,
}

/// Reset-in-place resend of one terminal outbound row (v2.1 §D1), routed to
/// the row's OWN peer's engine (the worker ignores other peers' rows):
///
/// 1. eligibility — `failed` / `cancelled` / `confirmed`; a receiver-declined
///    row is rejected with a pointer at the explicit divert action (autonomous
///    or casual retries must never override a human decline);
/// 2. payload — a manifest-only dir (post-confirm cleanup, or a fan-out dir
///    freed by the all-terminal coordinator) is rebuilt from the originals
///    ([`rebuild_package_payloads`]); a partial rebuild also swaps the row's
///    per-file rows to the restored subset;
/// 3. gate — the fan-out cleanup coordinator is re-armed: the reset row will
///    reach a SECOND terminal, which must widen the gate (or, after a cleanup,
///    re-open it for the rebuilt payload);
/// 4. reset — `reset_outbound_for_resend` (same row, `generation`+1, fresh
///    per-attempt wire id, files → pending);
/// 5. re-drive — `engine.resend(id)` on the peer's engine.
///
/// Ordering is crash-safe: a crash after the reset leaves a non-terminal row
/// the engine's own crash-resume re-announces on the next start.
pub async fn resend_in_place(
    store: &StandaloneSyncStore,
    engine: &SyncEngineHandle,
    cleanup: Option<&SharedPackageCleanup>,
    config: &Config,
    batches: &BatchStore,
    row: &OutboundRow,
) -> Result<ResendReport> {
    if is_declined(row) {
        bail!("declined by receiver — use \"Resend as new transfer\"");
    }
    if !matches!(
        row.state,
        OutboundState::Failed | OutboundState::Cancelled | OutboundState::Confirmed
    ) {
        bail!("not terminal");
    }

    let dir = PathBuf::from(&row.package_ref);
    let mut report = ResendReport::default();
    if !package_has_payload(&dir) {
        let rebuild = rebuild_package_payloads(&dir, config, batches)?;
        if !rebuild.is_full() {
            // The manifest shrank to the restored subset; the per-file rows
            // must follow it or the next announce offers files that are not on
            // disk. Best-effort: on failure the announce's manifest fallback
            // still serves the right subset, the rows are just stale.
            let records = read_manifest(&dir)
                .with_context(|| format!("re-read rebuilt manifest {}", dir.display()))?;
            if let Err(error) =
                store.replace_outbound_files(row.id, &pending_file_rows(row.id, &records))
            {
                tracing::warn!(%error, id = row.id, "per-file rows not updated to rebuilt subset");
            }
        }
        report.rebuilt = true;
        report.missing = rebuild.missing;
        report.changed = rebuild.changed;
    }

    // Widen (or, post-cleanup, re-open) the fan-out gate BEFORE the reset: the
    // moment the row is live again its terminal can race in, and an un-armed
    // gate would over-count against a still-pending sibling target.
    if let Some(coord) = cleanup {
        coord.rearm(&dir, 1);
    }

    let new_wire_id = uuid::Uuid::new_v4().to_string();
    store
        .reset_outbound_for_resend(row.id, &new_wire_id)
        .with_context(|| format!("reset outbound {} for resend", row.id))?;
    if let Err(error) = store.append_sync_event(
        athenaeum_core::sync::Direction::Sent,
        &row.id.to_string(),
        "resend",
        None,
    ) {
        tracing::warn!(%error, id = row.id, "append resend sync_event failed");
    }
    engine
        .resend(row.id)
        .await
        .with_context(|| format!("re-drive resend {}", row.id))?;
    tracing::info!(id = row.id, rebuilt = report.rebuilt, "transfer resent in place");
    Ok(report)
}

/// What a divert does with the SOURCE transfer's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceDisposition {
    /// The source is spent. A receiver decline is final per `batch_uuid`, so the
    /// declined identity has no future: its payload MOVES into the new one when
    /// no fan-out sibling still serves the dir, the `perseus_seen` retention
    /// linkage follows it, and the old row is stamped with the divert
    /// cross-link (which doubles as the double-click guard).
    Retire,
    /// The source stays live history — 0.5.1 §6, "the original row and its
    /// history are untouched". The new identity is materialized BESIDE it: the
    /// old dir is never written (its Files tab, its *Delete source files*
    /// affordance and any future in-place resend all still read it), its
    /// `perseus_seen` linkage keeps pointing at the batch that actually
    /// delivered those files, and its row state/error are left exactly as they
    /// are.
    Keep,
}

/// One accepted divert: the brand-new transfer and how much of the batch it
/// carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentBatch {
    /// The new outbound row's id (the worker minted it).
    pub new_id: i64,
    /// Files the new transfer actually carries.
    pub sent: usize,
    /// Manifest records dropped because their original is gone from disk, or
    /// was rewritten since (a changed capture file is a NEW snapshot, which the
    /// watcher enqueues on its own — never smuggled into a batch that promised
    /// different bytes).
    pub skipped: usize,
}

/// Why a divert could not be built.
#[derive(Debug)]
pub enum SendBatchError {
    /// The package dir was cleaned after delivery AND not one original capture
    /// file survives — there is nothing left to send. Nothing was touched.
    /// Callers map this to its own status (`409`), separately from a genuine
    /// failure: it is an answer about the batch, not a fault.
    AllSourcesGone(String),
    /// Anything else (unreadable manifest, failed copy, dead engine).
    Other(anyhow::Error),
}

impl std::fmt::Display for SendBatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendBatchError::AllSourcesGone(detail) => write!(f, "{detail}"),
            SendBatchError::Other(error) => write!(f, "{error:#}"),
        }
    }
}

// `source` is deliberately not forwarded: `Display` already prints the whole
// `Other` chain (`{error:#}`), so an anyhow wrapper — which is how
// `resend_declined_as_new` re-raises this — reads the same either way.
impl std::error::Error for SendBatchError {}

impl From<anyhow::Error> for SendBatchError {
    fn from(error: anyhow::Error) -> Self {
        SendBatchError::Other(error)
    }
}

/// Send an already-recorded batch as a brand-NEW transfer, to `engine`'s peer —
/// which may be a device this batch never went to (0.5.1 §6 *Send to device…*)
/// or the row's own peer (the declined re-ask, and the deliberate "ask them
/// again" — the receiver's dedup handshake decides what actually travels).
///
/// The payload gets a fresh package identity: new dir basename ⇒ new wire
/// `batch_uuid` ⇒ a brand-new inbound row on the chosen peer, keyed apart from
/// anything the source batch already established there.
///
/// **Where the bytes come from.** A package dir still holding its payload is
/// taken as-is; one already freed by post-confirm cleanup is rebuilt from the
/// original capture files through [`plan_package_rebuild`]. A rebuild that
/// cannot find every original is an **eligible subset**, not a failure: the new
/// manifest shrinks to what can actually be served and the drop count comes
/// back in [`SentBatch::skipped`]. Only a batch with *nothing* left is refused
/// ([`SendBatchError::AllSourcesGone`]).
///
/// **What happens to the source** is [`SourceDisposition`]'s call — and a
/// `Retire` still copies rather than moves when a fan-out sibling's row shares
/// the dir, because renaming would yank the payload out from under it.
///
/// **`layout`** is the receiver landing shape the new transfer asks for
/// (mirror-hierarchy T6), and it is the CALLER's decision rather than a re-read
/// of the setting, because the two callers legitimately want different answers:
/// *Send to device…* mints a transfer the operator is asking for now, so it takes
/// the current setting, while a declined re-ask
/// ([`resend_declined_as_new`]) passes `row.layout` — re-asking for the delivery
/// that was already described, not for whatever the setting says today.
#[allow(clippy::too_many_arguments)]
pub async fn send_batch_to_target(
    store: &StandaloneSyncStore,
    engine: &SyncEngineHandle,
    cleanup: Option<&SharedPackageCleanup>,
    config: &Config,
    batches: &BatchStore,
    seen: &SeenStore,
    row: &OutboundRow,
    disposition: SourceDisposition,
    layout: PackageLayout,
) -> Result<SentBatch, SendBatchError> {
    let retire = disposition == SourceDisposition::Retire;
    let old_ref = row.package_ref.clone();
    let old_dir = PathBuf::from(&old_ref);
    let parent = old_dir
        .parent()
        .with_context(|| format!("package dir has no parent: {old_ref}"))?
        .to_path_buf();
    let new_dir = parent.join(uuid::Uuid::new_v4().to_string());
    let new_ref = new_dir.to_string_lossy().into_owned();

    // Shared-dir probe: any OTHER outbound row on the same package dir means a
    // fan-out sibling still owns it — never rename it out from under them.
    let shared = store
        .all_outbound(u32::MAX)
        .context("scan outbound rows for dir ownership")?
        .iter()
        .any(|other| other.id != row.id && other.package_ref == old_ref);
    // Only a retired source hands its payload over; everything else is a copy
    // beside it.
    let moved = retire && !shared;

    let mut skipped = 0usize;
    if package_has_payload(&old_dir) {
        if moved {
            if let Err(e) = std::fs::rename(&old_dir, &new_dir) {
                return Err(SendBatchError::Other(if e.kind() == std::io::ErrorKind::NotFound {
                    anyhow::anyhow!(
                        "package data is missing on disk; cannot resend (already resent as a new transfer?)"
                    )
                } else {
                    anyhow::Error::from(e)
                        .context(format!("rename declined package dir {old_ref} -> {new_ref}"))
                }));
            }
        } else {
            copy_dir_recursive(&old_dir, &new_dir)
                .with_context(|| format!("copy package dir {old_ref} -> {new_ref}"))?;
        }
    } else {
        // Payload freed after delivery (or by the fan-out coordinator) — rebuild
        // from the originals. The plan is resolved against the OLD dir, which is
        // where the manifest and the `perseus_batch_files` linkage are keyed,
        // and applied into whichever dir the new transfer will serve from: a
        // KEPT source is never written to, so its manifest keeps naming the full
        // batch even when this send can only carry part of it.
        let plan = plan_package_rebuild(&old_dir, config, batches)?;
        if plan.sendable() == 0 {
            return Err(SendBatchError::AllSourcesGone(format!(
                "all {} source files are gone from disk ({} missing, {} changed)",
                plan.total(),
                plan.report.missing.len(),
                plan.report.changed.len()
            )));
        }
        skipped = plan.skipped();
        plan.apply(if moved { &old_dir } else { &new_dir })?;
        if moved {
            if let Err(e) = std::fs::rename(&old_dir, &new_dir) {
                return Err(SendBatchError::Other(anyhow::Error::from(e).context(
                    format!("rename rebuilt package dir {old_ref} -> {new_ref}"),
                )));
            }
        }
    }

    // The announce is built from the manifest the NEW dir actually serves — a
    // partial rebuild already shrank it, so this is the eligible subset.
    let records = read_manifest(&new_dir)
        .with_context(|| format!("read manifest for divert {}", new_dir.display()))?;
    let files: Vec<AnnounceFileEntry> = records
        .iter()
        .map(|record| AnnounceFileEntry {
            rel_path: record.rel_path.clone(),
            byte_size: record.byte_size,
            frame_uuid: record.frame_uuid.clone(),
        })
        .collect();
    let rel_paths: Vec<String> = records.iter().map(|r| r.rel_path.clone()).collect();
    let display_name = row
        .display_name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| {
            crate::run::batch_display_name(
                &rel_paths,
                &chrono::Utc::now().format("%Y-%m-%d").to_string(),
            )
        });

    // A copied dir is a fresh single-row package: gate its cleanup on that one
    // row, or its terminal would free a dir the coordinator never counted.
    if !moved {
        if let Some(coord) = cleanup {
            coord.register(&new_dir, 1);
        }
    }

    // Perseus bookkeeping — best-effort (a failure only degrades
    // retention/rebuild for the NEW transfer, the safe direction).
    // Only a RETIRED source hands its `perseus_seen` linkage over: retention
    // has to track a transfer that can still confirm, and the declined one
    // never will. A kept source keeps its own: those files were delivered under
    // IT, and re-pointing them at a fresh, unconfirmed transfer would silently
    // un-deliver them for retention.
    if retire {
        match seen.relink_package(&old_ref, &new_ref) {
            Ok(rows) => tracing::debug!(rows, "perseus_seen linkage moved to diverted transfer"),
            Err(error) => tracing::warn!(%error, "perseus_seen relink failed; retention may not track the new transfer"),
        }
    }
    if let Err(error) = batches.clone_files(&old_ref, &new_ref) {
        tracing::warn!(%error, "perseus_batch_files clone failed; a future rebuild falls back to reverse-mapping");
    }
    if let Err(error) = batches.record(
        &new_ref,
        "manual",
        &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        records.len(),
    ) {
        tracing::warn!(%error, "perseus_batch row for diverted transfer not recorded");
    }

    // The worker inserts the new row (+ per-file pending rows) in one
    // transaction and replies with its id — no pre-insert here.
    let new_id = match engine
        .enqueue_package(&new_dir, Some(display_name), files, layout)
        .await
    {
        Ok(id) => id,
        Err(e) => {
            // Undo so the source row's affordances stay live: put a moved
            // payload back under the old identity (and the linkage with it), or
            // drop the copy nobody will ever serve.
            if moved {
                if std::fs::rename(&new_dir, &old_dir).is_err() {
                    tracing::error!(old_ref = %old_ref, new_ref = %new_ref, "divert undo rename failed; payload dir left under the new name");
                }
            } else {
                let _ = std::fs::remove_dir_all(&new_dir);
                if let Err(error) = batches.delete(&new_ref) {
                    tracing::warn!(%error, "divert undo: batch rows for the dropped copy not removed");
                }
            }
            if retire {
                if let Err(error) = seen.relink_package(&new_ref, &old_ref) {
                    tracing::warn!(%error, "divert undo: perseus_seen relink-back failed");
                }
            }
            return Err(anyhow::Error::from(e)
                .context("enqueue diverted transfer")
                .into());
        }
    };

    // Journal cross-links, all best-effort: the new transfer is already durably
    // queued. The declined re-ask also stamps the old row's error with the
    // `— resent as new transfer #N` suffix, which both shows the divert in the
    // UI and makes a double-click bounce off the strict [`is_declined`] guard;
    // a KEPT source's row is never written to.
    if retire {
        if let Err(error) = store.set_last_error(
            row.id,
            Some(&format!(
                "{CANCELLED_BY_RECEIVER_DETAIL} — resent as new transfer #{new_id}"
            )),
        ) {
            tracing::warn!(%error, id = row.id, "divert suffix not recorded on the declined row");
        }
    }
    let (old_detail, new_detail) = if retire {
        (
            format!("as_new_transfer={new_id}"),
            format!("of_declined={}", row.id),
        )
    } else {
        (
            format!("sent_to_device={new_id}"),
            format!("of_batch={}", row.id),
        )
    };
    let _ = store.append_sync_event(
        athenaeum_core::sync::Direction::Sent,
        &row.id.to_string(),
        "resend",
        Some(&old_detail),
    );
    let _ = store.append_sync_event(
        athenaeum_core::sync::Direction::Sent,
        &new_id.to_string(),
        "resend",
        Some(&new_detail),
    );
    tracing::info!(
        old_id = row.id,
        new_id,
        moved,
        shared,
        sent = records.len(),
        skipped,
        new_ref = %new_ref,
        "batch diverted to a new transfer"
    );
    Ok(SentBatch {
        new_id,
        sent: records.len(),
        skipped,
    })
}

/// Divert a **receiver-declined** transfer into a brand-NEW transfer (the
/// explicit operator re-ask; autonomous retries never come here), re-asking the
/// row's OWN peer: [`send_batch_to_target`] with the declined identity
/// [retired](SourceDisposition::Retire).
///
/// The gate is strict on purpose — only a receiver-declined row comes here, and
/// only once (the divert's own suffix breaks [`is_declined`] for a second
/// click). Everything else routes to [`resend_in_place`] (same `batch_uuid`) or,
/// for a deliberate send elsewhere, to [`send_batch_to_target`] with
/// [`SourceDisposition::Keep`].
///
/// The new transfer CLONES the declined row's own `layout` rather than reading
/// the current `mirror_hierarchy` setting (mirror-hierarchy T6): this is one
/// delivery re-asked, and a setting flipped since the decline must not silently
/// re-shape where the receiver lands files the operator already described.
#[allow(clippy::too_many_arguments)]
pub async fn resend_declined_as_new(
    store: &StandaloneSyncStore,
    engine: &SyncEngineHandle,
    cleanup: Option<&SharedPackageCleanup>,
    config: &Config,
    batches: &BatchStore,
    seen: &SeenStore,
    row: &OutboundRow,
) -> Result<i64> {
    if !is_declined(row) {
        bail!("only a receiver-declined transfer can be resent as a new transfer");
    }
    let done = send_batch_to_target(
        store,
        engine,
        cleanup,
        config,
        batches,
        seen,
        row,
        SourceDisposition::Retire,
        row.layout,
    )
    .await?;
    Ok(done.new_id)
}

/// Copy `src`'s regular files (and subdirectories) into `dst`, creating `dst`.
/// Only the node types a package dir can contain (the writer creates files +
/// parent dirs; nothing else) — anything exotic is skipped.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("create {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry.with_context(|| format!("read entry in {}", src.display()))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry
            .file_type()
            .with_context(|| format!("stat {}", from.display()))?;
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to)
                .with_context(|| format!("copy {} -> {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use athenaeum_core::package::{write_package, PayloadKind, MANIFEST_VERSION};

    /// A config whose `capture_dirs` are exactly `dirs` (created on disk) and
    /// whose `data_dir` is a sibling temp dir.
    fn config_with_dirs(root: &Path, dirs: &[&Path]) -> Config {
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
        for d in dirs {
            std::fs::create_dir_all(d).unwrap();
        }
        let list = dirs
            .iter()
            .map(|d| format!("\"{}\"", d.display()))
            .collect::<Vec<_>>()
            .join(", ");
        let toml = format!(
            "capture_dirs = [{list}]\ndata_dir=\"{}\"\npairing_ticket=\"t\"\nmode=\"auto\"\n[retention]\npolicy=\"keep_everything\"\ndry_run=true\n",
            data.display()
        );
        Config::from_toml_str(&toml).unwrap()
    }

    fn record_for(src: &Path, rel_path: &str, uuid: &str) -> ManifestRecord {
        ManifestRecord {
            v: MANIFEST_VERSION,
            frame_uuid: uuid.to_string(),
            origin_catalog_uuid: uuid.to_string(),
            origin_device: "test-device".to_string(),
            payload_kind: PayloadKind::RawFrame,
            rel_path: rel_path.to_string(),
            byte_size: std::fs::metadata(src).unwrap().len(),
            xxh3: package::xxh3_full_file(src).unwrap(),
            frame_meta: serde_json::json!({}),
            analysis: None,
            app_version: "test".to_string(),
            project: None,
        }
    }

    /// Build a real package at `pkg_dir` from `(source, rel_path)` pairs via the
    /// production writer, then strip its payload copies the way the engine's
    /// post-confirm cleanup does (every non-manifest file removed).
    fn build_and_clean_pkg(pkg_dir: &Path, sources: &[(&Path, &str)]) -> Vec<ManifestRecord> {
        let records: Vec<(std::path::PathBuf, ManifestRecord)> = sources
            .iter()
            .enumerate()
            .map(|(i, (src, rel))| ((*src).to_path_buf(), record_for(src, rel, &format!("u-{i}"))))
            .collect();
        let manifest: Vec<ManifestRecord> = records.iter().map(|(_, r)| r.clone()).collect();
        write_package(pkg_dir, records).unwrap();
        clean_payloads(pkg_dir);
        manifest
    }

    /// The engine's cleanup shape: keep the manifest, remove everything else.
    fn clean_payloads(pkg_dir: &Path) {
        for entry in std::fs::read_dir(pkg_dir).unwrap().flatten() {
            if entry.file_name() == std::ffi::OsStr::new(MANIFEST_FILENAME) {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                std::fs::remove_dir_all(entry.path()).unwrap();
            } else {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }
        assert!(!package_has_payload(pkg_dir), "cleanup left payload behind");
    }

    fn batches(root: &Path) -> BatchStore {
        BatchStore::open(root.join("perseus.db")).unwrap()
    }

    /// Resolution via the durable `perseus_batch_files` linkage: the original
    /// lives OUTSIDE every capture dir (reverse-mapping can't find it), so a
    /// successful rebuild proves the table was used.
    #[test]
    fn rebuild_resolves_source_via_batch_files_linkage() {
        let tmp = tempfile::tempdir().unwrap();
        let cap = tmp.path().join("cap");
        let config = config_with_dirs(tmp.path(), &[&cap]);
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let src = elsewhere.join("light-1.fits");
        std::fs::write(&src, b"linkage-payload").unwrap();

        let pkg = tmp.path().join("pkg-linkage");
        build_and_clean_pkg(&pkg, &[(&src, "light-1.fits")]);
        let store = batches(tmp.path());
        store
            .record_files(&pkg.to_string_lossy(), &[("light-1.fits".into(), src.clone())])
            .unwrap();

        let report = rebuild_package_payloads(&pkg, &config, &store).unwrap();
        assert_eq!(report.restored, vec!["light-1.fits"]);
        assert!(report.is_full());
        assert_eq!(
            std::fs::read(pkg.join("light-1.fits")).unwrap(),
            b"linkage-payload",
            "payload bytes restored from the linked source"
        );
    }

    /// No linkage rows (a pre-upgrade batch): a single capture dir resolves a
    /// bare rel_path directly.
    #[test]
    fn rebuild_reverse_maps_single_capture_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let cap = tmp.path().join("cap");
        let config = config_with_dirs(tmp.path(), &[&cap]);
        let src = cap.join("light-2.fits");
        std::fs::write(&src, b"reverse-map-payload").unwrap();

        let pkg = tmp.path().join("pkg-reverse");
        build_and_clean_pkg(&pkg, &[(&src, "light-2.fits")]);

        let report = rebuild_package_payloads(&pkg, &config, &batches(tmp.path())).unwrap();
        assert!(report.is_full());
        assert_eq!(
            std::fs::read(pkg.join("light-2.fits")).unwrap(),
            b"reverse-map-payload"
        );
    }

    /// No linkage rows, TWO capture dirs: the rel_path carries the sanitized
    /// dir-label prefix (see `run::compute_rel_path`), which the reverse-map
    /// strips for the dir it names.
    #[test]
    fn rebuild_reverse_maps_labeled_multi_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let obs_a = tmp.path().join("Obs-A");
        let obs_b = tmp.path().join("obs-b");
        let config = config_with_dirs(tmp.path(), &[&obs_a, &obs_b]);
        let src = obs_a.join("night1").join("light-3.fits");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, b"labeled-payload").unwrap();

        // The label the batch builder would have prefixed: sanitize("Obs-A").
        let rel = "obs-a/night1/light-3.fits";
        let pkg = tmp.path().join("pkg-labeled");
        build_and_clean_pkg(&pkg, &[(&src, rel)]);

        let report = rebuild_package_payloads(&pkg, &config, &batches(tmp.path())).unwrap();
        assert!(report.is_full(), "label-stripped reverse-map found the source: {report:?}");
        assert_eq!(std::fs::read(pkg.join(rel)).unwrap(), b"labeled-payload");
    }

    /// A changed original (same path, different bytes) is EXCLUDED and reported
    /// — a changed capture file is a new snapshot, not this batch's content —
    /// and the manifest shrinks to the restored subset.
    #[test]
    fn rebuild_skips_changed_and_reports() {
        let tmp = tempfile::tempdir().unwrap();
        let cap = tmp.path().join("cap");
        let config = config_with_dirs(tmp.path(), &[&cap]);
        let keep = cap.join("keep.fits");
        let drift = cap.join("drift.fits");
        std::fs::write(&keep, b"stable-bytes").unwrap();
        std::fs::write(&drift, b"original-bytes").unwrap();

        let pkg = tmp.path().join("pkg-changed");
        build_and_clean_pkg(&pkg, &[(&keep, "keep.fits"), (&drift, "drift.fits")]);
        std::fs::write(&drift, b"REWRITTEN-after-packaging").unwrap();

        let report = rebuild_package_payloads(&pkg, &config, &batches(tmp.path())).unwrap();
        assert_eq!(report.restored, vec!["keep.fits"]);
        assert_eq!(report.changed, vec!["drift.fits"]);
        assert!(report.missing.is_empty());
        assert!(
            !pkg.join("drift.fits").exists(),
            "the changed file's new bytes must NOT be smuggled into the batch"
        );
        let manifest = read_manifest(&pkg).unwrap();
        assert_eq!(
            manifest.iter().map(|r| r.rel_path.as_str()).collect::<Vec<_>>(),
            vec!["keep.fits"],
            "the manifest honestly shrank to the restored subset"
        );
    }

    /// A vanished original is reported `missing` and the manifest rewritten to
    /// the restored subset (the /api/sent row honestly shrinks; the full audit
    /// stays in sync_history).
    #[test]
    fn rebuild_partial_rewrites_manifest_to_restored_subset() {
        let tmp = tempfile::tempdir().unwrap();
        let cap = tmp.path().join("cap");
        let config = config_with_dirs(tmp.path(), &[&cap]);
        let a = cap.join("a.fits");
        let b = cap.join("b.fits");
        std::fs::write(&a, b"aaaa").unwrap();
        std::fs::write(&b, b"bbbb").unwrap();

        let pkg = tmp.path().join("pkg-partial");
        build_and_clean_pkg(&pkg, &[(&a, "a.fits"), (&b, "b.fits")]);
        std::fs::remove_file(&b).unwrap();

        let report = rebuild_package_payloads(&pkg, &config, &batches(tmp.path())).unwrap();
        assert_eq!(report.restored, vec!["a.fits"]);
        assert_eq!(report.missing, vec!["b.fits"]);
        assert_eq!(read_manifest(&pkg).unwrap().len(), 1);
        assert!(pkg.join("a.fits").exists());
    }

    /// Zero restorable files refuse BEFORE any mutation: the manifest keeps all
    /// its records and no payload copies appear.
    #[test]
    fn rebuild_zero_restorable_errors_without_mutation() {
        let tmp = tempfile::tempdir().unwrap();
        let cap = tmp.path().join("cap");
        let config = config_with_dirs(tmp.path(), &[&cap]);
        let a = cap.join("gone.fits");
        std::fs::write(&a, b"soon-gone").unwrap();

        let pkg = tmp.path().join("pkg-zero");
        build_and_clean_pkg(&pkg, &[(&a, "gone.fits")]);
        std::fs::remove_file(&a).unwrap();

        let err = rebuild_package_payloads(&pkg, &config, &batches(tmp.path())).unwrap_err();
        assert!(
            format!("{err:#}").contains("could be restored"),
            "honest zero-restorable error, got {err:#}"
        );
        assert_eq!(read_manifest(&pkg).unwrap().len(), 1, "manifest untouched");
        assert!(!package_has_payload(&pkg), "no partial payload appeared");
    }

    /// A second rebuild over an already-restored dir is a clean no-op pass:
    /// everything reports `restored` (already present) and the bytes survive.
    #[test]
    fn rebuild_is_idempotent_on_double_run() {
        let tmp = tempfile::tempdir().unwrap();
        let cap = tmp.path().join("cap");
        let config = config_with_dirs(tmp.path(), &[&cap]);
        let src = cap.join("x.fits");
        std::fs::write(&src, b"xxxx-bytes").unwrap();

        let pkg = tmp.path().join("pkg-idem");
        build_and_clean_pkg(&pkg, &[(&src, "x.fits")]);

        let store = batches(tmp.path());
        let first = rebuild_package_payloads(&pkg, &config, &store).unwrap();
        let second = rebuild_package_payloads(&pkg, &config, &store).unwrap();
        assert_eq!(first, second, "the second pass is byte-for-byte the same report");
        assert_eq!(std::fs::read(pkg.join("x.fits")).unwrap(), b"xxxx-bytes");
    }

    // ── the declined-transfer divert ──────────────────────────────────────────

    use athenaeum_core::sharing::loopback::LoopbackNetwork;
    use athenaeum_core::sharing::SharingTransport;
    use athenaeum_core::sync::{PackageCleanupSink, SyncEngine};
    use std::sync::Arc;

    const PEER: [u8; 32] = [0xAB; 32];

    /// Build a payload-intact package dir + its declined outbound row, over a
    /// live loopback engine (no receiver — enqueue durably queues, which is all
    /// the divert needs). Returns everything the divert call takes.
    struct DivertHarness {
        _tmp: tempfile::TempDir,
        config: Config,
        store: Arc<StandaloneSyncStore>,
        seen: SeenStore,
        batches: BatchStore,
        engine: SyncEngineHandle,
        pkg: std::path::PathBuf,
        src: std::path::PathBuf,
        row_id: i64,
    }

    fn divert_harness() -> DivertHarness {
        divert_harness_with(PackageLayout::Batch)
    }

    /// [`divert_harness`] whose declined row carries `layout` — the stamp the
    /// original transfer went out with, which the divert must clone.
    fn divert_harness_with(layout: PackageLayout) -> DivertHarness {
        let tmp = tempfile::tempdir().unwrap();
        let cap = tmp.path().join("cap");
        let config = config_with_dirs(tmp.path(), &[&cap]);
        let src = cap.join("declined.fits");
        std::fs::write(&src, b"declined-payload").unwrap();

        let pkg = tmp.path().join("packages").join("old-uuid");
        let rec = record_for(&src, "declined.fits", "u-declined");
        write_package(&pkg, vec![(src.clone(), rec)]).unwrap();

        let store =
            Arc::new(StandaloneSyncStore::open(tmp.path().join("perseus.db")).unwrap());
        let seen = SeenStore::open(tmp.path().join("perseus.db")).unwrap();
        let batches = BatchStore::open(tmp.path().join("perseus.db")).unwrap();
        let row_id = store
            .enqueue(&pkg.to_string_lossy(), PEER, None, &[], layout)
            .unwrap();
        store.set_state(row_id, OutboundState::Cancelled).unwrap();
        store
            .set_last_error(row_id, Some(CANCELLED_BY_RECEIVER_DETAIL))
            .unwrap();
        let m = std::fs::metadata(&src).unwrap();
        seen.mark_enqueued(
            &src,
            m.len(),
            crate::seen::mtime_millis(m.modified().ok()),
            &pkg.to_string_lossy(),
        )
        .unwrap();
        batches
            .record_files(&pkg.to_string_lossy(), &[("declined.fits".into(), src.clone())])
            .unwrap();

        let transport: Arc<dyn SharingTransport> = Arc::new(LoopbackNetwork::new().endpoint());
        let engine = SyncEngine::spawn(
            Arc::clone(&store) as Arc<dyn athenaeum_core::sync::store::SyncStore>,
            transport,
            PEER,
        );
        DivertHarness {
            _tmp: tmp,
            config,
            store,
            seen,
            batches,
            engine,
            pkg,
            src,
            row_id,
        }
    }

    /// Sole-owner divert: atomic rename into a fresh uuid basename, the seen /
    /// batch-files linkage follows, the old row gains the double-click-guard
    /// suffix, and a second divert of the same row is rejected.
    #[tokio::test]
    async fn divert_declined_sole_owner_renames_relinks_and_guards_double_click() {
        let h = divert_harness();
        let row = h.store.get_outbound(h.row_id).unwrap().unwrap();
        let new_id = resend_declined_as_new(
            &h.store, &h.engine, None, &h.config, &h.batches, &h.seen, &row,
        )
        .await
        .expect("divert succeeds");
        assert_ne!(new_id, h.row_id, "the divert mints a brand-new transfer");

        // The payload moved to the new identity (sole owner ⇒ rename).
        assert!(!h.pkg.exists(), "the old dir was renamed away");
        let new_row = h.store.get_outbound(new_id).unwrap().expect("new row exists");
        assert_ne!(new_row.package_ref, h.pkg.to_string_lossy());
        let new_dir = std::path::PathBuf::from(&new_row.package_ref);
        assert!(package_has_payload(&new_dir), "payload + manifest travelled");
        assert!(
            new_row.display_name.is_some(),
            "the diverted transfer carries a human batch name"
        );

        // Bookkeeping followed the payload.
        assert_eq!(
            h.seen
                .source_for_package(&new_row.package_ref)
                .unwrap()
                .map(|l| l.path),
            Some(h.src.clone()),
            "the live seen linkage points at the new package"
        );
        assert!(
            !h.batches.files_for(&new_row.package_ref).unwrap().is_empty(),
            "the rel_path → source linkage was cloned"
        );

        // The old row is history with the guard suffix…
        let old = h.store.get_outbound(h.row_id).unwrap().unwrap();
        assert_eq!(old.state, OutboundState::Cancelled);
        let err = old.last_error.unwrap();
        assert!(err.starts_with(CANCELLED_BY_RECEIVER_DETAIL), "prefix keeps the UI rendering");
        assert!(err.contains(&format!("resent as new transfer #{new_id}")));

        // …and a double-click bounces off the strict guard.
        let old_again = h.store.get_outbound(h.row_id).unwrap().unwrap();
        let second = resend_declined_as_new(
            &h.store, &h.engine, None, &h.config, &h.batches, &h.seen, &old_again,
        )
        .await;
        assert!(second.is_err(), "a second divert of the same row is rejected");

        h.engine.shutdown().await;
    }

    /// Mirror-hierarchy T6: the divert CLONES the declined transfer's own layout
    /// stamp — it never re-reads the current setting. The operator's re-ask is the
    /// delivery they already asked for, so a `mirror_hierarchy` turned off since
    /// (as here: the config says batch) must not silently re-shape where the
    /// receiver lands those files.
    #[tokio::test]
    async fn divert_declined_clones_the_source_rows_layout() {
        let h = divert_harness_with(PackageLayout::Mirror);
        assert!(
            !h.config.mirror_hierarchy,
            "the harness config asks for batch — the row's own stamp must win"
        );
        let row = h.store.get_outbound(h.row_id).unwrap().unwrap();
        assert_eq!(
            row.layout,
            PackageLayout::Mirror,
            "the declined row's own stamp"
        );

        let new_id = resend_declined_as_new(
            &h.store, &h.engine, None, &h.config, &h.batches, &h.seen, &row,
        )
        .await
        .expect("divert succeeds");

        assert_eq!(
            h.store.get_outbound(new_id).unwrap().unwrap().layout,
            PackageLayout::Mirror,
            "the new transfer keeps the original's landing shape"
        );

        h.engine.shutdown().await;
    }

    /// Fan-out divert: a package dir shared with a sibling target's row is
    /// COPIED (never renamed) — the old dir stays live for the sibling — and
    /// the new dir is registered with the cleanup coordinator as a one-target
    /// gate (its terminal cleans it once).
    #[tokio::test]
    async fn divert_shared_dir_copies_and_keeps_sibling_payload() {
        let h = divert_harness();
        // A sibling target's row on the SAME package dir.
        let sibling_peer: [u8; 32] = [0xCD; 32];
        h.store
            .enqueue(
                &h.pkg.to_string_lossy(),
                sibling_peer,
                None,
                &[],
                PackageLayout::Batch,
            )
            .unwrap();
        let coord = Arc::new(SharedPackageCleanup::new());
        coord.register(&h.pkg, 2);

        let row = h.store.get_outbound(h.row_id).unwrap().unwrap();
        let new_id = resend_declined_as_new(
            &h.store,
            &h.engine,
            Some(&coord),
            &h.config,
            &h.batches,
            &h.seen,
            &row,
        )
        .await
        .expect("fan-out divert succeeds");

        assert!(
            package_has_payload(&h.pkg),
            "the shared dir is untouched — the sibling target still serves from it"
        );
        let new_row = h.store.get_outbound(new_id).unwrap().unwrap();
        let new_dir = std::path::PathBuf::from(&new_row.package_ref);
        assert!(package_has_payload(&new_dir), "the copy carries the payload");
        assert_eq!(
            std::fs::read(new_dir.join("declined.fits")).unwrap(),
            std::fs::read(h.pkg.join("declined.fits")).unwrap(),
            "byte-identical copy"
        );

        // The new dir was registered expected=1: its single terminal cleans it.
        coord.on_terminal(&new_dir);
        assert!(
            !package_has_payload(&new_dir),
            "the diverted dir's own gate cleans it after its one terminal"
        );
        assert!(
            package_has_payload(&h.pkg),
            "…without ever touching the sibling's shared dir"
        );

        h.engine.shutdown().await;
    }

    // ── §6 send a recorded batch to another device ────────────────────────────

    const PEER_B: [u8; 32] = [0xCD; 32];

    /// A DELIVERED batch the way the operator meets it in the Transfers history:
    /// a confirmed outbound row whose package dir has been cleaned back to its
    /// manifest, with the `perseus_batch_files` linkage and the `perseus_seen`
    /// rows the real send path leaves behind — plus a second peer's engine to
    /// divert it to.
    struct SendToHarness {
        _tmp: tempfile::TempDir,
        config: Config,
        store: Arc<StandaloneSyncStore>,
        seen: SeenStore,
        batches: BatchStore,
        /// The source row's own peer.
        engine_a: SyncEngineHandle,
        /// The divert target — a device this batch never went to.
        engine_b: SyncEngineHandle,
        pkg: std::path::PathBuf,
        sources: Vec<std::path::PathBuf>,
        row_id: i64,
    }

    impl SendToHarness {
        fn row(&self) -> OutboundRow {
            self.store.get_outbound(self.row_id).unwrap().unwrap()
        }
        async fn shutdown(self) {
            self.engine_a.shutdown().await;
            self.engine_b.shutdown().await;
        }
    }

    /// `rels` are capture-dir-relative names; each becomes a source file, a
    /// manifest record and a linkage row.
    ///
    /// `delivered` picks which terminal shape the history row has, and the two
    /// differ on disk exactly as they do in production: a **confirmed** batch's
    /// payload copies are gone (post-confirm cleanup — the engine even heals
    /// leftovers at startup, so a confirmed row with payload is not a state that
    /// exists), a **failed** one still holds them.
    fn send_to_harness(rels: &[&str], delivered: bool) -> SendToHarness {
        let tmp = tempfile::tempdir().unwrap();
        let cap = tmp.path().join("cap");
        let config = config_with_dirs(tmp.path(), &[&cap]);
        let sources: Vec<std::path::PathBuf> = rels
            .iter()
            .enumerate()
            .map(|(i, rel)| {
                let src = cap.join(rel);
                std::fs::write(&src, format!("payload-{i}-{rel}").as_bytes()).unwrap();
                src
            })
            .collect();

        let pkg = tmp.path().join("packages").join("sent-uuid");
        let pairs: Vec<(&Path, &str)> = sources
            .iter()
            .zip(rels.iter())
            .map(|(src, rel)| (src.as_path(), *rel))
            .collect();
        if delivered {
            build_and_clean_pkg(&pkg, &pairs);
        } else {
            let records: Vec<(std::path::PathBuf, ManifestRecord)> = pairs
                .iter()
                .enumerate()
                .map(|(i, (src, rel))| {
                    ((*src).to_path_buf(), record_for(src, rel, &format!("u-{i}")))
                })
                .collect();
            write_package(&pkg, records).unwrap();
        }

        let store = Arc::new(StandaloneSyncStore::open(tmp.path().join("perseus.db")).unwrap());
        let seen = SeenStore::open(tmp.path().join("perseus.db")).unwrap();
        let batches = BatchStore::open(tmp.path().join("perseus.db")).unwrap();
        let pkg_ref = pkg.to_string_lossy().into_owned();
        let row_id = store
            .enqueue(&pkg_ref, PEER, None, &[], PackageLayout::Batch)
            .unwrap();
        if delivered {
            store.confirm(row_id, &[]).unwrap();
        } else {
            store.set_state(row_id, OutboundState::Failed).unwrap();
        }
        for src in &sources {
            let m = std::fs::metadata(src).unwrap();
            seen.mark_enqueued(
                src,
                m.len(),
                crate::seen::mtime_millis(m.modified().ok()),
                &pkg_ref,
            )
            .unwrap();
        }
        batches
            .record_files(
                &pkg_ref,
                &sources
                    .iter()
                    .zip(rels.iter())
                    .map(|(src, rel)| ((*rel).to_string(), src.clone()))
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        batches.record(&pkg_ref, "auto", "2026-07-26T00:00:00.000Z", rels.len()).unwrap();

        let spawn = |peer: [u8; 32]| {
            let transport: Arc<dyn SharingTransport> = Arc::new(LoopbackNetwork::new().endpoint());
            SyncEngine::spawn(
                Arc::clone(&store) as Arc<dyn athenaeum_core::sync::store::SyncStore>,
                transport,
                peer,
            )
        };
        let engine_a = spawn(PEER);
        let engine_b = spawn(PEER_B);
        SendToHarness {
            _tmp: tmp,
            config,
            store,
            seen,
            batches,
            engine_a,
            engine_b,
            pkg,
            sources,
            row_id,
        }
    }

    /// The §6 happy path: a confirmed, cleaned batch is sent to a SECOND device
    /// as a brand-new transfer — new row on that peer, fresh `batch_uuid` (a new
    /// dir basename), payload rebuilt from the originals — and the source batch
    /// is left exactly as it was: same state, no error stamp, still
    /// manifest-only, still holding its own retention linkage.
    #[tokio::test]
    async fn send_to_second_peer_mints_a_new_transfer_and_leaves_the_source_untouched() {
        let h = send_to_harness(&["a.fits", "b.fits"], true);
        let row = h.row();
        let old_ref = row.package_ref.clone();

        let done = send_batch_to_target(
            &h.store,
            &h.engine_b,
            None,
            &h.config,
            &h.batches,
            &h.seen,
            &row,
            SourceDisposition::Keep,
            PackageLayout::Batch,
        )
        .await
        .expect("the divert succeeds");

        assert_ne!(done.new_id, h.row_id, "a brand-new transfer, never a reset");
        assert_eq!((done.sent, done.skipped), (2, 0), "the whole batch travels");

        let new_row = h.store.get_outbound(done.new_id).unwrap().unwrap();
        assert_eq!(new_row.peer, PEER_B, "queued on the TARGET peer's engine");
        assert_ne!(new_row.package_ref, old_ref, "a fresh package identity");
        let new_dir = std::path::PathBuf::from(&new_row.package_ref);
        assert_ne!(
            new_dir.file_name(),
            std::path::Path::new(&old_ref).file_name(),
            "a fresh dir basename IS the fresh wire batch_uuid"
        );
        assert!(package_has_payload(&new_dir), "payload rebuilt into the new dir");
        assert_eq!(read_manifest(&new_dir).unwrap().len(), 2);
        for (rel, src) in ["a.fits", "b.fits"].iter().zip(h.sources.iter()) {
            assert_eq!(
                std::fs::read(new_dir.join(rel)).unwrap(),
                std::fs::read(src).unwrap(),
                "{rel} carries the original bytes"
            );
        }

        // …and the source batch is untouched, on disk and in the DB.
        let old = h.store.get_outbound(h.row_id).unwrap().unwrap();
        assert_eq!(old.state, OutboundState::Confirmed);
        assert_eq!(old.last_error, None, "no divert stamp on a kept source");
        assert_eq!(old.package_ref, old_ref, "the dir was never renamed away");
        assert!(
            !package_has_payload(&h.pkg),
            "the delivered batch's dir stays cleaned — nothing was restored into it"
        );
        assert_eq!(read_manifest(&h.pkg).unwrap().len(), 2, "old manifest intact");
        assert_eq!(
            h.seen.source_for_package(&old_ref).unwrap().map(|l| l.path),
            Some(h.sources[0].clone()),
            "retention still tracks the batch that actually delivered these files"
        );
        assert!(
            h.seen.source_for_package(&new_row.package_ref).unwrap().is_none(),
            "…and was not re-pointed at the unconfirmed new transfer"
        );

        // The new transfer carries its own bookkeeping (rebuild + history).
        assert_eq!(
            h.batches.files_for(&new_row.package_ref).unwrap().len(),
            2,
            "the rel_path → source linkage was cloned"
        );
        assert!(
            h.batches
                .list()
                .unwrap()
                .iter()
                .any(|b| b.package_ref == new_row.package_ref),
            "the new batch shows in the transfers history"
        );

        h.shutdown().await;
    }

    /// Eligible subset (owner rule: act on the eligible portion, report `N of
    /// M`): one source deleted since the batch shipped is dropped and counted,
    /// the other two travel — and the SOURCE batch's manifest still names all
    /// three, because a kept source is never rewritten.
    #[tokio::test]
    async fn send_to_sends_the_eligible_subset_when_a_source_was_deleted() {
        let h = send_to_harness(&["a.fits", "b.fits", "c.fits"], true);
        std::fs::remove_file(&h.sources[1]).unwrap();
        let row = h.row();

        let done = send_batch_to_target(
            &h.store,
            &h.engine_b,
            None,
            &h.config,
            &h.batches,
            &h.seen,
            &row,
            SourceDisposition::Keep,
            PackageLayout::Batch,
        )
        .await
        .expect("a partial batch still sends");

        assert_eq!((done.sent, done.skipped), (2, 1), "2 of 3, one deleted locally");
        let new_dir =
            std::path::PathBuf::from(h.store.get_outbound(done.new_id).unwrap().unwrap().package_ref);
        let manifest = read_manifest(&new_dir).unwrap();
        assert_eq!(
            manifest.iter().map(|r| r.rel_path.as_str()).collect::<Vec<_>>(),
            vec!["a.fits", "c.fits"],
            "the new package advertises only what it can serve"
        );
        assert!(!new_dir.join("b.fits").exists());
        assert_eq!(
            read_manifest(&h.pkg).unwrap().len(),
            3,
            "the source batch still records the full three-file delivery"
        );

        h.shutdown().await;
    }

    /// Every source deleted → a typed refusal (the route's `409`), raised before
    /// anything is built: no new dir, no new row, source manifest intact.
    #[tokio::test]
    async fn send_to_refuses_when_every_source_is_gone() {
        let h = send_to_harness(&["a.fits", "b.fits"], true);
        for src in &h.sources {
            std::fs::remove_file(src).unwrap();
        }
        let before = h.store.all_outbound(u32::MAX).unwrap().len();
        let row = h.row();

        let err = send_batch_to_target(
            &h.store,
            &h.engine_b,
            None,
            &h.config,
            &h.batches,
            &h.seen,
            &row,
            SourceDisposition::Keep,
            PackageLayout::Batch,
        )
        .await
        .expect_err("nothing left to send");

        assert!(
            matches!(err, SendBatchError::AllSourcesGone(_)),
            "the all-gone answer is its own variant, not a generic failure: {err}"
        );
        assert_eq!(
            h.store.all_outbound(u32::MAX).unwrap().len(),
            before,
            "no transfer was minted"
        );
        assert_eq!(read_manifest(&h.pkg).unwrap().len(), 2, "source manifest intact");
        let siblings = std::fs::read_dir(h.pkg.parent().unwrap()).unwrap().count();
        assert_eq!(siblings, 1, "no half-built package dir was left behind");

        h.shutdown().await;
    }

    /// A batch whose dir still holds its payload (here: a FAILED transfer the
    /// operator re-routes to another device) is COPIED into the new identity —
    /// the source keeps serving its own bytes for its Files tab, its *Delete
    /// source files* and a later in-place resend.
    #[tokio::test]
    async fn send_to_copies_an_intact_package_and_leaves_the_original_servable() {
        let h = send_to_harness(&["a.fits"], false);
        let row = h.row();

        let done = send_batch_to_target(
            &h.store,
            &h.engine_b,
            None,
            &h.config,
            &h.batches,
            &h.seen,
            &row,
            SourceDisposition::Keep,
            PackageLayout::Batch,
        )
        .await
        .expect("the divert succeeds");

        assert_eq!((done.sent, done.skipped), (1, 0));
        assert!(
            package_has_payload(&h.pkg),
            "the source dir was copied, never moved"
        );
        let new_dir =
            std::path::PathBuf::from(h.store.get_outbound(done.new_id).unwrap().unwrap().package_ref);
        assert_eq!(
            std::fs::read(new_dir.join("a.fits")).unwrap(),
            std::fs::read(h.pkg.join("a.fits")).unwrap(),
            "byte-identical copy"
        );

        h.shutdown().await;
    }

    /// Mirror-hierarchy T6: the transfer a *Send to device…* mints carries the
    /// layout the CALLER chose (the route passes the current `mirror_hierarchy`
    /// setting), not the source row's stamp — so a batch that originally shipped
    /// as `batch` lands in the mirror tree once the operator has switched over.
    #[tokio::test]
    async fn send_to_stamps_the_layout_the_caller_chose() {
        let h = send_to_harness(&["a.fits"], true);
        let row = h.row();
        assert_eq!(
            row.layout,
            PackageLayout::Batch,
            "the source transfer shipped as batch"
        );

        let done = send_batch_to_target(
            &h.store,
            &h.engine_b,
            None,
            &h.config,
            &h.batches,
            &h.seen,
            &row,
            SourceDisposition::Keep,
            PackageLayout::Mirror,
        )
        .await
        .expect("the divert succeeds");

        assert_eq!(
            h.store.get_outbound(done.new_id).unwrap().unwrap().layout,
            PackageLayout::Mirror,
            "the new transfer asks for the layout this send chose"
        );

        h.shutdown().await;
    }

    /// Sending back to the row's OWN peer is allowed: it is an explicit operator
    /// re-ask, and the fresh `batch_uuid` gets it a brand-new inbound row there
    /// (the receiver's dedup handshake then decides what actually travels).
    #[tokio::test]
    async fn send_to_the_same_peer_is_an_allowed_re_ask() {
        let h = send_to_harness(&["a.fits"], true);
        let row = h.row();

        let done = send_batch_to_target(
            &h.store,
            &h.engine_a,
            None,
            &h.config,
            &h.batches,
            &h.seen,
            &row,
            SourceDisposition::Keep,
            PackageLayout::Batch,
        )
        .await
        .expect("re-asking the same device is allowed");

        let new_row = h.store.get_outbound(done.new_id).unwrap().unwrap();
        assert_eq!(new_row.peer, PEER, "same device…");
        assert_ne!(new_row.package_ref, h.pkg.to_string_lossy(), "…new batch_uuid");

        h.shutdown().await;
    }
}
