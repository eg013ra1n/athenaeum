//! Primary-side receive pipeline (Stage I, task A7): turn a fetched
//! [`package`](crate::package) into catalog rows and per-frame receipts.
//!
//! [`ingest_package`] is the synchronous heart of the receiver. Given a package
//! directory that a peer served and we fetched (manifest + payload files) plus
//! an [`incoming_root`] to land files under, it walks the manifest and, per
//! frame, runs the spec §9 order:
//!
//! 1. **Verify** — re-hash the payload (full-content xxh3) against the manifest
//!    record. A mismatch (or a missing/corrupt payload) yields a
//!    [`Rejected`](ReceiptOutcome::Rejected) receipt and no catalog write.
//! 2. **Dedup by `frames.uuid`** — a frame we already hold (same uuid) is never
//!    overwritten (primary-wins, B9): if our copy's `updated_at` is newer than
//!    the arriving snapshot's, the history outcome is `skipped_older`; otherwise
//!    `duplicate`. Either way the receipt is [`Duplicate`](ReceiptOutcome::Duplicate)
//!    and nothing is written to `files`/`frames`. A same-uuid frame whose content
//!    hash differs is logged at `warn` and still kept (v1 keeps existing).
//! 3. **Dedup by content hash** — a payload whose full-content xxh3 was already
//!    recorded as [`Ingested`](ReceiptOutcome::Ingested) in `sync_receipts` for
//!    some earlier frame is also a `Duplicate`, no write. This compares the
//!    manifest's *full* xxh3 (already verified in step 1) against previously
//!    ingested full hashes — **not** `files.content_hash` (`duplicates::compute_xxhash`
//!    3-position sampling hash, intentional for fast move-verify but wrong for
//!    a dedupe *decision*: two distinct files can share all three sampled
//!    windows and differ only outside them, which would falsely mark a
//!    genuinely new frame `Duplicate` and lose it). Caveat: a frame the
//!    **scanner** ingested (not via sync) has no full hash recorded anywhere,
//!    so it is not a secondary-dedupe candidate — `frames.uuid` (step 2)
//!    remains the primary key for those; acceptable v1.
//! 4. **Ingest** — otherwise land the payload (tmp/rename into
//!    `<incoming_root>/<sender_slug>/<rel_path>`, mirroring the sender's tree
//!    under a slug derived from the *authenticated* peer node id) and insert `files` +
//!    `frames` + `fits_header` rows (reusing the scanner primitives), carrying
//!    the manifest's `frame_uuid` onto `frames.uuid` so a later redelivery
//!    dedups. Receipt [`Ingested`](ReceiptOutcome::Ingested). If the catalog
//!    write fails after the payload has already been landed, the landed file
//!    is removed (best-effort) so a failed frame never leaves an orphan with
//!    no catalog row pointing at it.
//!
//! Every frame — ingested, duplicate, or rejected — writes a `sync_receipts` row
//! (the ack-replay log) and a `sync_history` row (`direction = received`). All
//! writes for one frame happen in a single transaction on the caller-supplied
//! connection, so the receipt/history never drift from the catalog rows.
//!
//! # Redelivery (partial/rejected ack)
//!
//! The sender only confirms a package once every receipt is non-`Rejected`
//! (task A7 fix-review). A package with a rejected frame therefore gets
//! re-announced (redelivered) by the sender's normal retry path. On redelivery,
//! [`ingest_package`] does **not** blindly reprocess every frame: for each
//! manifest record it first checks the existing `sync_receipts` row for
//! `(package_id, frame_uuid)`. A prior `Ingested`/`Duplicate` receipt is reused
//! verbatim (no re-hash I/O, and — importantly — no risk of an already-ingested
//! frame being re-classified as `Duplicate` of itself on the next pass); only a
//! frame with no receipt yet, or a prior `Rejected` receipt, is actually
//! reprocessed through [`process_frame`]. This is what makes "repair the source
//! file, then redeliver" converge to exactly one ingest per frame (see
//! `ingest_tests::transit_corruption_repaired_then_redelivery_confirms`).
//!
//! This module is deliberately ungated: it depends only on `db`, `package`,
//! `sharing`, `fits_parser`, `duplicates`, and `models`, so it compiles in the
//! headless (`--no-default-features`) build.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::models::{File, FileFormat, Frame};
use crate::package::{self, ManifestRecord};
use crate::sharing::types::{FrameReceipt, PackageAnnounce, ReceiptOutcome};

use super::now_iso;
use super::store::{insert_history_row, insert_receipt, load_receipts, receipt_outcome_to_db};
use super::models::{Direction, HistoryRow};

/// Aggregate result of ingesting one package: the per-frame receipts to ack back
/// to the sender, plus a breakdown by outcome for the `sync-finished` event.
#[derive(Debug, Clone, Default)]
pub struct IngestOutcome {
    /// Receipts to return in the ack, one per manifest frame.
    pub receipts: Vec<FrameReceipt>,
    pub ingested: u32,
    pub duplicate: u32,
    pub skipped_older: u32,
    pub rejected: u32,
}

impl IngestOutcome {
    /// Frames the receiver accepted without error (ingested + every flavour of
    /// duplicate). The complement of [`failed`](Self::failed).
    pub fn ok_count(&self) -> u32 {
        self.ingested + self.duplicate + self.skipped_older
    }

    /// Frames the receiver rejected (integrity failure / unusable metadata).
    pub fn failed(&self) -> u32 {
        self.rejected
    }
}

/// Per-frame verdict returned by [`process_frame`]: the receipt for the ack plus
/// the short outcome tag recorded in `sync_history.outcome`.
struct FrameVerdict {
    receipt: FrameReceipt,
    history_outcome: &'static str,
}

/// Ingest one fetched package into the catalog on `conn`, landing accepted
/// payloads under `incoming_root`. `peer_device` is the sending node id (hex),
/// stamped as `sync_history.peer_device`. Returns the receipts to ack.
///
/// Idempotent: re-running against the same catalog produces `Duplicate` receipts
/// and no new `files`/`frames` rows (the per-frame uuid/content dedup), so a lost
/// ack that triggers a resend is safe even without the receiver's package-level
/// replay guard.
pub fn ingest_package(
    conn: &Connection,
    incoming_root: &Path,
    package_dir: &Path,
    announce: &PackageAnnounce,
    peer_device: &str,
) -> Result<IngestOutcome> {
    let records = package::read_manifest(package_dir)
        .with_context(|| format!("read manifest for ingest {}", package_dir.display()))?;

    tracing::info!(
        package_id = %announce.package_id.0,
        count = records.len(),
        src = %peer_device,
        "sync ingest start"
    );

    let started_at = now_iso();
    let mut outcome = IngestOutcome::default();

    let package_id = &announce.package_id.0;

    // Per-sender landing prefix, computed once (the whole package is from one
    // authenticated peer). Derived from `peer_device` — the AUTHENTICATED node id
    // (hex) the receiver verified — NOT the manifest's wire-supplied
    // `origin_device`, which a peer could set to anything.
    let sender_slug = sanitize_slug(peer_device);

    // Redelivery optimization: load whatever this package already has on
    // record and reuse any non-Rejected receipt verbatim instead of
    // reprocessing. This avoids needless re-hash I/O AND avoids a subtle
    // correctness trap: reprocessing an already-Ingested frame would re-run
    // the uuid dedup, find the frame it itself inserted, and reclassify it as
    // `Duplicate` — losing the original `Ingested` receipt for no reason. Only
    // a frame with no receipt yet, or a prior `Rejected` one, is reprocessed.
    let existing_receipts: HashMap<String, FrameReceipt> = load_receipts(conn, package_id)?
        .into_iter()
        .map(|r| (r.frame_uuid.clone(), r))
        .collect();

    for record in &records {
        if let Some(prior) = existing_receipts.get(&record.frame_uuid) {
            if !matches!(prior.outcome, ReceiptOutcome::Rejected(_)) {
                tracing::debug!(
                    frame_uuid = %record.frame_uuid,
                    "sync ingest: reusing prior non-rejected receipt (redelivery skip)"
                );
                match prior.outcome {
                    ReceiptOutcome::Ingested => outcome.ingested += 1,
                    ReceiptOutcome::Duplicate => outcome.duplicate += 1,
                    ReceiptOutcome::Rejected(_) => unreachable!("guarded above"),
                }
                outcome.receipts.push(prior.clone());
                continue;
            }
            // Prior receipt was Rejected: fall through and actually reprocess
            // — the source may have been repaired since the last attempt.
        }

        let verdict = match process_frame(conn, incoming_root, package_dir, record, package_id, peer_device, &sender_slug, &started_at) {
            Ok(v) => v,
            Err(e) => {
                // A processing error (I/O, DB) is surfaced as a Rejected receipt
                // rather than aborting the whole batch — one bad frame must not
                // strand its siblings. Logged, never swallowed.
                tracing::error!(
                    package_id = %package_id,
                    frame_uuid = %record.frame_uuid,
                    error = %format!("{e:#}"),
                    "sync ingest frame failed"
                );
                let receipt = FrameReceipt {
                    frame_uuid: record.frame_uuid.clone(),
                    xxh3: record.xxh3.clone(),
                    outcome: ReceiptOutcome::Rejected(format!("{e:#}")),
                };
                // Best-effort receipt/history so the failure is still durable and
                // the ack carries a verdict for this frame.
                let _ = record_receipt_and_history(
                    conn, package_id, &receipt, record, peer_device, &started_at, "rejected",
                );
                FrameVerdict { receipt, history_outcome: "rejected" }
            }
        };

        match verdict.history_outcome {
            "ingested" => outcome.ingested += 1,
            "skipped_older" => outcome.skipped_older += 1,
            "rejected" => outcome.rejected += 1,
            _ => outcome.duplicate += 1,
        }
        outcome.receipts.push(verdict.receipt);
    }

    tracing::info!(
        package_id = %announce.package_id.0,
        ingested = outcome.ingested,
        duplicate = outcome.duplicate,
        skipped_older = outcome.skipped_older,
        rejected = outcome.rejected,
        "sync ingest done"
    );
    Ok(outcome)
}

/// Process one manifest record end to end and return its verdict. All DB writes
/// happen inside a single transaction on `conn`.
fn process_frame(
    conn: &Connection,
    incoming_root: &Path,
    package_dir: &Path,
    record: &ManifestRecord,
    package_id: &str,
    peer_device: &str,
    sender_slug: &str,
    started_at: &str,
) -> Result<FrameVerdict> {
    // Guard the record's rel_path (untrusted, wire-supplied) before joining.
    package::validate_rel_path(&record.rel_path)
        .with_context(|| format!("reject unsafe rel_path {}", record.rel_path))?;
    let payload = package_dir.join(&record.rel_path);

    // 1. Verify integrity: full-content xxh3 must match the manifest.
    let actual = match package::xxh3_full_file(&payload) {
        Ok(h) => h,
        Err(e) => {
            let receipt = rejected_receipt(record, format!("payload unreadable: {e}"));
            record_receipt_and_history(conn, package_id, &receipt, record, peer_device, started_at, "rejected")?;
            return Ok(FrameVerdict { receipt, history_outcome: "rejected" });
        }
    };
    if actual != record.xxh3 {
        tracing::warn!(frame_uuid = %record.frame_uuid, "sync ingest xxh3 mismatch; rejecting");
        let receipt = rejected_receipt(record, format!("xxh3 mismatch: manifest {}, disk {actual}", record.xxh3));
        record_receipt_and_history(conn, package_id, &receipt, record, peer_device, started_at, "rejected")?;
        return Ok(FrameVerdict { receipt, history_outcome: "rejected" });
    }

    // Deserialize the Frame snapshot early — needed for the landing date, dedup
    // comparison, and (on ingest) the row itself. A malformed snapshot is a
    // reject, not a panic.
    let snapshot: Frame = serde_json::from_value(record.frame_meta.clone())
        .context("deserialize frame_meta snapshot")?;

    // 2. Dedup by frames.uuid (primary-wins).
    if let Some(existing) = find_frame_by_uuid(conn, &record.frame_uuid)? {
        let history_outcome = primary_wins_outcome(existing.updated_at.as_deref(), snapshot.updated_at.as_deref());

        // Same uuid but different bytes → keep existing, flag it. (v1: no
        // overwrite; documented in the module header.) Advisory only — this
        // sampling-hash comparison never decides a skip (that would repeat the
        // fix-review's sampling-collision loss risk); it exists purely to log a
        // signal for operators, comparing against the same `files.content_hash`
        // sampling hash the scanner's own Duplicates view already stores.
        if let Ok(incoming_hash) = crate::duplicates::compute_xxhash(&payload) {
            if existing.content_hash.as_deref() != Some(incoming_hash.as_str()) {
                tracing::warn!(
                    frame_uuid = %record.frame_uuid,
                    "sync ingest: same uuid, different content — keeping existing (v1)"
                );
            }
        }

        let receipt = duplicate_receipt(record);
        record_receipt_and_history(conn, package_id, &receipt, record, peer_device, started_at, history_outcome)?;
        tracing::debug!(frame_uuid = %record.frame_uuid, outcome = history_outcome, "sync ingest duplicate by uuid");
        return Ok(FrameVerdict { receipt, history_outcome });
    }

    // 3. Dedup by FULL content hash (same bytes, different/absent uuid).
    // Compares the manifest's full-content xxh3 (`record.xxh3`, already
    // verified against the payload in step 1) against previously *ingested*
    // full hashes recorded in `sync_receipts` — deliberately NOT
    // `files.content_hash` (the 3-position sampling hash): two distinct files
    // can share every sampled window and differ only outside them, which would
    // falsely mark a genuinely new frame `Duplicate` and lose it (see module
    // docs). A frame the scanner ingested directly (no full hash on record) is
    // not a secondary-dedupe candidate here — `frames.uuid` (step 2) remains
    // the primary key for those; acceptable v1.
    if full_hash_already_ingested(conn, &record.xxh3)? {
        tracing::debug!(frame_uuid = %record.frame_uuid, "sync ingest duplicate by full content hash");
        let receipt = duplicate_receipt(record);
        record_receipt_and_history(conn, package_id, &receipt, record, peer_device, started_at, "duplicate")?;
        return Ok(FrameVerdict { receipt, history_outcome: "duplicate" });
    }

    // 4. Ingest: land the payload, then insert catalog rows in one transaction.
    let landed = land_payload(incoming_root, &payload, record, sender_slug)
        .with_context(|| format!("land payload {}", record.rel_path))?;
    // The sampling hash is computed here purely to populate `files.content_hash`
    // (the unrelated scanner-side Duplicates view) — it never decides a skip.
    let sampling_hash = crate::duplicates::compute_xxhash(&landed)
        .with_context(|| format!("hash landed payload {}", landed.display()))?;

    let receipt = ingested_receipt(record);
    let write_result: Result<()> = (|| {
        let tx = conn.unchecked_transaction().context("begin ingest tx")?;
        insert_ingested_rows(&tx, &landed, record, &snapshot, &sampling_hash)?;
        insert_receipt(&tx, package_id, &receipt, started_at)?;
        insert_history_row(&tx, &received_history(record, &snapshot, peer_device, started_at, "ingested"))?;
        tx.commit().context("commit ingest tx")
    })();

    if let Err(e) = write_result {
        // The file was already landed (renamed into its final location) before
        // the transaction ran; a failed insert/commit must not leave it behind
        // with no catalog row pointing at it. Best-effort cleanup — logged,
        // never masks the original error.
        if let Err(rm_err) = std::fs::remove_file(&landed) {
            tracing::warn!(
                path = %landed.display(), error = %rm_err,
                "sync ingest: failed to remove orphaned landed file after tx error"
            );
        }
        return Err(e).with_context(|| format!("ingest catalog write for frame {}", record.frame_uuid));
    }

    tracing::info!(frame_uuid = %record.frame_uuid, path = %landed.display(), "sync ingest frame landed");
    Ok(FrameVerdict { receipt, history_outcome: "ingested" })
}

/// The minimal existing-frame projection dedup needs.
struct ExistingFrame {
    updated_at: Option<String>,
    content_hash: Option<String>,
}

/// Look up an existing frame by its uuid, LEFT-joining its file's content hash.
/// A LEFT JOIN (not INNER) so a frame is still detected as existing even if its
/// file row is somehow absent — dedup by uuid must never silently miss and then
/// attempt an INSERT that would violate the `frames.uuid` unique index.
fn find_frame_by_uuid(conn: &Connection, frame_uuid: &str) -> Result<Option<ExistingFrame>> {
    conn.query_row(
        "SELECT fr.updated_at, f.content_hash
         FROM frames fr LEFT JOIN files f ON fr.file_id = f.id
         WHERE fr.uuid = ?1 LIMIT 1",
        params![frame_uuid],
        |r| Ok(ExistingFrame { updated_at: r.get(0)?, content_hash: r.get(1)? }),
    )
    .optional()
    .context("dedup lookup by frames.uuid")
}

/// True if `xxh3` (a manifest's full-content hash, already verified against
/// its payload in step 1) was already recorded as
/// [`Ingested`](ReceiptOutcome::Ingested) for some earlier frame. Queries
/// `sync_receipts`, deliberately NOT `files.content_hash` — see the module
/// docs and the step-3 comment at the call site for why the sampling hash is
/// unsafe to use for this decision.
fn full_hash_already_ingested(conn: &Connection, xxh3: &str) -> Result<bool> {
    let ingested = receipt_outcome_to_db(&ReceiptOutcome::Ingested);
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_receipts WHERE xxh3 = ?1 AND outcome = ?2",
            params![xxh3, ingested],
            |r| r.get(0),
        )
        .context("dedup lookup by full content hash")?;
    Ok(n > 0)
}

/// Primary-wins decision: if our existing copy's `updated_at` is strictly newer
/// than the arriving snapshot's, the local edit wins (`skipped_older`);
/// otherwise it is an ordinary `duplicate`. RFC3339-millis text compares
/// lexically in timestamp order, so the string comparison is correct.
fn primary_wins_outcome(existing: Option<&str>, snapshot: Option<&str>) -> &'static str {
    match (existing, snapshot) {
        (Some(e), Some(s)) if e > s => "skipped_older",
        _ => "duplicate",
    }
}

/// Land an accepted payload mirroring the sender's tree under
/// `<incoming_root>/<sender_slug>/<rel_path>`, tmp-copy + atomic rename,
/// collision-suffixed. `sender_slug` is derived from the AUTHENTICATED peer node
/// id (Plan 2B); `rel_path` is `validate_rel_path`-guarded in `process_frame`, so
/// the join cannot escape `<incoming_root>/<sender_slug>/`. Returns the final path.
fn land_payload(
    incoming_root: &Path,
    payload: &Path,
    record: &ManifestRecord,
    sender_slug: &str,
) -> Result<PathBuf> {
    let dest = unique_path(&incoming_root.join(sender_slug).join(Path::new(&record.rel_path)));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create landing dir {}", parent.display()))?;
    }

    // tmp + atomic rename: copy to a sibling temp, then rename into place.
    // The staging payload lives under the same incoming_root, so this is normally
    // an intra-filesystem move; the copy handles the cross-device case too.
    let tmp = dest.with_extension(format!(
        "{}.tmp",
        dest.extension().and_then(|e| e.to_str()).unwrap_or("part")
    ));
    std::fs::copy(payload, &tmp)
        .with_context(|| format!("copy payload to {}", tmp.display()))?;
    std::fs::rename(&tmp, &dest)
        .with_context(|| format!("rename landed file into {}", dest.display()))?;
    Ok(dest)
}

/// Insert the `files` + `fits_header` + `frames` rows for an ingested frame,
/// carrying the manifest `frame_uuid` onto `frames.uuid`. Runs on `tx`.
fn insert_ingested_rows(
    tx: &Connection,
    landed: &Path,
    record: &ManifestRecord,
    snapshot: &Frame,
    content_hash: &str,
) -> Result<()> {
    let format = format_of(&record.rel_path);
    let now = chrono::Utc::now();
    let modified_at = snapshot.date_obs.unwrap_or(now);

    let file = File {
        id: None,
        path: landed.to_string_lossy().to_string(),
        filename: filename_of(&record.rel_path),
        size: record.byte_size as i64,
        modified_at,
        format: format.clone(),
        created_at: now,
        metadata_hash: None,
        content_hash: Some(content_hash.to_string()),
        archived_in_operation: None,
        archive_zip_path: None,
        archive_path_in_zip: None,
        uuid: None,
        updated_at: None,
    };
    let file_id = crate::db::insert_file(tx, &file).context("insert files row")?;

    // Re-extract the raw header from the landed file for the metadata-pane revert
    // blob. The manifest carries only the parsed Frame snapshot, not header text.
    let header = match format {
        FileFormat::FITS => crate::fits_parser::extract_fits_header(landed),
        FileFormat::XISF => crate::fits_parser::extract_xisf_header(landed),
    };
    match header {
        Ok(text) => {
            crate::db::insert_fits_header(tx, file_id, &text).context("insert fits_header row")?;
        }
        Err(e) => {
            // Non-fatal: a missing header only disables per-field revert for this
            // frame. Insert an empty blob so the row still exists.
            tracing::warn!(frame_uuid = %record.frame_uuid, error = %e, "sync ingest header extract failed");
            crate::db::insert_fits_header(tx, file_id, "").context("insert empty fits_header row")?;
        }
    }

    // Insert the frame from the snapshot, then stamp the manifest identity. The
    // scanner primitive omits the uuid/updated_at columns, so the identity
    // trigger fills a fresh uuid on INSERT; the follow-up UPDATE replaces it with
    // the origin's frame_uuid (and preserves the snapshot's updated_at) so a
    // later redelivery dedups by uuid.
    let mut frame = snapshot.clone();
    frame.id = None;
    frame.file_id = file_id;
    let frame_id = crate::db::insert_frame(tx, &frame).context("insert frames row")?;
    let updated_at = snapshot.updated_at.clone().unwrap_or_else(now_iso);
    tx.execute(
        "UPDATE frames SET uuid = ?1, updated_at = ?2 WHERE id = ?3",
        params![record.frame_uuid, updated_at, frame_id],
    )
    .context("stamp frame uuid/updated_at")?;

    // Carry the analysis summary through when the origin had one.
    if let Some(analysis_value) = &record.analysis {
        match serde_json::from_value::<crate::models::FrameAnalysis>(analysis_value.clone()) {
            Ok(mut a) => {
                a.id = None;
                a.frame_id = frame_id;
                a.file_id = file_id;
                if let Err(e) = crate::db::analysis::upsert_frame_analysis(tx, &a) {
                    tracing::warn!(frame_uuid = %record.frame_uuid, error = %e, "sync ingest analysis upsert failed");
                }
            }
            Err(e) => tracing::warn!(frame_uuid = %record.frame_uuid, error = %e, "sync ingest analysis decode failed"),
        }
    }

    Ok(())
}

/// Write the receipt + history rows for one frame in a single transaction.
/// Used by the duplicate/rejected paths (the ingest path writes them inline in
/// its own transaction alongside the catalog rows).
fn record_receipt_and_history(
    conn: &Connection,
    package_id: &str,
    receipt: &FrameReceipt,
    record: &ManifestRecord,
    peer_device: &str,
    started_at: &str,
    history_outcome: &str,
) -> Result<()> {
    let snapshot: Frame = serde_json::from_value(record.frame_meta.clone()).unwrap_or_default();
    let tx = conn.unchecked_transaction().context("begin receipt tx")?;
    insert_receipt(&tx, package_id, receipt, started_at)?;
    insert_history_row(&tx, &received_history(record, &snapshot, peer_device, started_at, history_outcome))?;
    tx.commit().context("commit receipt tx")?;
    Ok(())
}

/// Build a `direction = received` history row for a frame.
fn received_history(
    record: &ManifestRecord,
    snapshot: &Frame,
    peer_device: &str,
    started_at: &str,
    outcome: &str,
) -> HistoryRow {
    HistoryRow {
        frame_uuid: record.frame_uuid.clone(),
        filename: filename_of(&record.rel_path),
        object: snapshot.object.clone(),
        peer_device: peer_device.to_string(),
        direction: Direction::Received,
        bytes: record.byte_size,
        started_at: started_at.to_string(),
        finished_at: Some(now_iso()),
        outcome: outcome.to_string(),
        project: record.project.as_ref().map(|p| p.project_id.clone()),
    }
}

fn ingested_receipt(record: &ManifestRecord) -> FrameReceipt {
    FrameReceipt { frame_uuid: record.frame_uuid.clone(), xxh3: record.xxh3.clone(), outcome: ReceiptOutcome::Ingested }
}
fn duplicate_receipt(record: &ManifestRecord) -> FrameReceipt {
    FrameReceipt { frame_uuid: record.frame_uuid.clone(), xxh3: record.xxh3.clone(), outcome: ReceiptOutcome::Duplicate }
}
fn rejected_receipt(record: &ManifestRecord, reason: String) -> FrameReceipt {
    FrameReceipt { frame_uuid: record.frame_uuid.clone(), xxh3: record.xxh3.clone(), outcome: ReceiptOutcome::Rejected(reason) }
}

/// Basename of a forward-slash manifest `rel_path`.
fn filename_of(rel_path: &str) -> String {
    rel_path.rsplit('/').next().unwrap_or(rel_path).to_string()
}

/// A filesystem-safe single path segment derived from a display string (the
/// per-sender landing-folder prefix). Lowercase; any char outside
/// `[a-z0-9._-]` → `-`; runs of `-` collapse; leading/trailing `-`/`.` trimmed;
/// capped at 24 chars; empty → `"node"`. Applied to the authenticated peer node
/// id here (Plan 2C applies it to a resolved device name instead).
pub(crate) fn sanitize_slug(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(24));
    let mut prev_dash = false;
    for ch in raw.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            out.push(c);
            prev_dash = false;
        } else {
            if !prev_dash {
                out.push('-');
            }
            prev_dash = true;
        }
        if out.len() >= 24 {
            break;
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '.');
    if trimmed.is_empty() {
        "node".to_string()
    } else {
        trimmed.to_string()
    }
}

/// File format inferred from a rel_path extension (XISF else FITS).
fn format_of(rel_path: &str) -> FileFormat {
    match rel_path.rsplit('.').next().map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("xisf") => FileFormat::XISF,
        _ => FileFormat::FITS,
    }
}

/// Return `base` if free, else `base` with a `_2`, `_3`, … suffix inserted before
/// the extension, until an unused path is found. `pub(crate)` so the sibling
/// [`project_ingest`](super::project_ingest) landing path reuses the exact
/// collision-suffix logic.
pub(crate) fn unique_path(base: &Path) -> PathBuf {
    if !base.exists() {
        return base.to_path_buf();
    }
    let parent = base.parent().unwrap_or_else(|| Path::new("."));
    let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = base.extension().and_then(|e| e.to_str());
    for n in 2..10_000 {
        let name = match ext {
            Some(ext) => format!("{stem}_{n}.{ext}"),
            None => format!("{stem}_{n}"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    // Absurd fallback (10k collisions): append the current nanos.
    let ts = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    parent.join(format!("{stem}_{ts}"))
}
