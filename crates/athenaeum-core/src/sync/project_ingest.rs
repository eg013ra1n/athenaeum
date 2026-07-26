//! Receive-side ingest for a Stage-II collaboration PROJECT package (slice 4,
//! task 5).
//!
//! Where personal-sync [`ingest_package`](super::ingest::ingest_package) turns a
//! fetched package into catalog `files`/`frames` rows, a *project* package never
//! enters the catalog: its frames are **contributions** — they land under a
//! managed per-project root and are tracked only in `project_contributions`
//! (Task-3 db layer). [`ingest_project_package`] is the synchronous heart of that
//! flow, called from the receiver's `ProjectAnnounceReceived` arm on a blocking
//! thread after the package has been fetched into `staging_dir`.
//!
//! # Fail-closed anchoring (Д2)
//!
//! The package's `project_packages` row (with its **hub-anchored**
//! `manifest_xxh3`) must already exist — the receiver arm refreshes announcements
//! before calling us, and we hard-error if the project or package row is missing
//! or the anchor is NULL. Before *any* manifest record is parsed we re-hash the
//! staged `manifest.ndjson` and compare it to `manifest_xxh3`: a re-authored
//! manifest (different frame set, tampered stamps) is rejected wholesale here,
//! not frame by frame.
//!
//! # Per-frame verdicts
//!
//! With the manifest anchored, each record is verified independently
//! (`validate_rel_path`, payload present, full-content xxh3, project stamp
//! cross-check). A failure yields a per-frame [`Rejected`](ReceiptOutcome::Rejected)
//! receipt and the batch continues. An accepted frame supersedes any prior
//! contribution the same publisher published under the same `frame_uuid`
//! (Д9) — the stale landed file is removed best-effort — then lands tmp-copy +
//! atomic rename under `<root>/<project-slug>/<publisher-slug>/<rel_path>` and
//! inserts one `project_contributions` row. A byte-identical re-delivery (same
//! `(project, publisher, uuid)` and same xxh3) is a [`Duplicate`](ReceiptOutcome::Duplicate)
//! and lands nothing.
//!
//! The publisher slug is **HUB-anchored** (`project_packages.publisher_display`),
//! never the serving peer and never the manifest's `origin_device` — the serving
//! peer (`peer_device`) is recorded only in `sync_history`.
//!
//! This module is ungated: it depends only on `db`, `package`, `sharing`, and the
//! sibling sync modules, so it compiles in the headless (`--no-default-features`)
//! build.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::db::collab_exchange::ContributionRow;
use crate::package::{self, ManifestRecord};
use crate::sharing::types::{FrameReceipt, ReceiptOutcome};

use super::ingest::{sanitize_slug, unique_path, IngestConn};
use super::models::{Direction, HistoryRow};
use super::now_iso;
use super::store::insert_history_row;

/// Aggregate result of ingesting one project package: the per-frame receipts to
/// ack back to the serving peer, the count accepted without an integrity error
/// (ingested + duplicate), and the frame uuids that were rejected.
#[derive(Debug, Clone)]
pub struct ProjectIngestOutcome {
    pub receipts: Vec<FrameReceipt>,
    pub ok_count: usize,
    pub failed: Vec<String>,
}

/// Ingest a fetched PROJECT package from `staging_dir`. Fail-closed: the package
/// row (with its hub-anchored `manifest_xxh3`) must already exist.
///
/// - `project_id` — the hub project id (also stamped in each record's `project`).
/// - `package_id` — the **HUB** package uuid (the `project_packages` row key,
///   audit B1). Never a path component here (contributions land under sanitized
///   project/publisher slugs), so it is used only as a db key.
/// - `peer_device` — the authenticated serving peer (hex), recorded in
///   `sync_history` only; it is NOT the landing slug (the publisher slug is
///   hub-anchored, Д5).
pub fn ingest_project_package(
    conn: IngestConn<'_>,
    staging_dir: &Path,
    project_id: &str,
    package_id: &str,
    peer_device: &str,
) -> Result<ProjectIngestOutcome> {
    // 1. Load the project (slug) and the hub-anchored package row. Missing either
    //    is a hard error — the receiver arm refreshes announcements first, so an
    //    unknown row here means a genuinely unknown package (fail-closed). Both
    //    reads share ONE connection acquisition (W2 T2.1), released before the
    //    manifest anchoring below (pure file I/O) runs.
    let (project, package) = conn.with(|c| -> Result<_> {
        let project = crate::db::collab::get_project(c, project_id)?
            .ok_or_else(|| anyhow!("unknown collaboration project {project_id}"))?;
        let package = crate::db::collab_exchange::get_package(c, package_id)?
            .ok_or_else(|| anyhow!("unknown collaboration package {package_id}"))?;
        Ok((project, package))
    })?;

    // A hub-rejected package must never re-land: a rejected contributor could
    // otherwise re-push the rejected content onto the coordinator (the poll
    // re-creates the row at state='rejected' because a coordinator sees every
    // state). Hard-refuse before any frame lands (I2).
    if package.state == "rejected" {
        bail!("refusing to ingest package {package_id}: hub state is 'rejected'");
    }

    let manifest_xxh3 = package
        .manifest_xxh3
        .clone()
        .ok_or_else(|| anyhow!("announcement carries no manifest anchor"))?;

    // 2. Anchor the manifest BEFORE parsing any record: the staged
    //    `manifest.ndjson` must hash to the hub-recorded `manifest_xxh3`. A
    //    mismatch rejects a re-authored manifest wholesale (Д2).
    let manifest_path = staging_dir.join(package::MANIFEST_FILENAME);
    let actual_anchor = package::xxh3_full_file(&manifest_path)
        .with_context(|| format!("hash staged manifest {}", manifest_path.display()))?;
    if actual_anchor != manifest_xxh3 {
        bail!(
            "manifest anchor mismatch for package {package_id}: hub {manifest_xxh3}, staged {actual_anchor}"
        );
    }
    // Retained bytes for byte-identical re-serving (Д2), stored on success.
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read staged manifest {}", manifest_path.display()))?;

    let records = package::read_manifest(staging_dir)
        .with_context(|| format!("read project manifest {}", staging_dir.display()))?;

    // 3. Landing root: the designated `collaboration` scan root, else a
    //    `<sync_dir>/collaboration` fallback derived from the staging path
    //    (`<sync_dir>/staging/<wire_package_id>`), mirroring the incoming-resolver
    //    fallback idiom.
    let landing_root = match conn.with(|c| crate::db::scan_root_path_of_kind(c, "collaboration"))? {
        Some(p) => PathBuf::from(p),
        None => staging_dir
            .parent()
            .and_then(|p| p.parent())
            .unwrap_or(staging_dir)
            .join("collaboration"),
    };

    tracing::info!(
        project_id,
        package_id,
        count = records.len(),
        src = %peer_device,
        "project ingest start"
    );

    let started_at = now_iso();
    let mut receipts: Vec<FrameReceipt> = Vec::with_capacity(records.len());
    let mut ingested_count = 0usize;
    let mut ok_count = 0usize;
    let mut failed: Vec<String> = Vec::new();

    for record in &records {
        // ONE connection acquisition per frame (W2 T2.1): the guard spans this
        // frame whole — dedup lookup → hash → supersede → land → contribution
        // transaction — and is released before the next frame, so a concurrent
        // lane waits one frame, never the whole (multi-GB) package.
        let receipt = conn.with(|c| {
            match process_project_frame(
                c,
                &landing_root,
                staging_dir,
                record,
                &project.slug,
                &package.publisher_display,
                project_id,
                package_id,
                peer_device,
                &started_at,
            ) {
                Ok(r) => r,
                Err(e) => {
                    // A processing error (I/O, DB) becomes a Rejected receipt rather
                    // than aborting the batch — one bad frame must not strand its
                    // siblings. Logged, never swallowed.
                    tracing::error!(
                        project_id,
                        package_id,
                        frame_uuid = %record.frame_uuid,
                        error = %format!("{e:#}"),
                        "project ingest frame failed"
                    );
                    rejected_receipt(record, format!("{e:#}"))
                }
            }
        });
        match &receipt.outcome {
            ReceiptOutcome::Ingested => {
                ingested_count += 1;
                ok_count += 1;
            }
            ReceiptOutcome::Duplicate => ok_count += 1,
            ReceiptOutcome::Rejected(_) => failed.push(record.frame_uuid.clone()),
            // Project ingest never produces a `Cancelled` verdict: it is the
            // receiver's per-frame ingest outcome (ingest/duplicate/reject), not
            // the sender-facing receiver-cancel wire value. Defensive no-op — a
            // cancelled frame is neither accepted (ok) nor failed here.
            ReceiptOutcome::Cancelled => {}
        }
        receipts.push(receipt);
    }

    // 4. On ≥1 landed frame, retain the (already anchor-verified) manifest bytes
    //    so this node can re-serve the package byte-identically (Д2). The local
    //    status reflects whether every frame was accepted.
    //    Epilogue: both writes under ONE final connection acquisition (W2 T2.1).
    let status = if failed.is_empty() { "complete" } else { "failed" };
    conn.with(|c| -> Result<()> {
        if ingested_count >= 1 {
            crate::db::collab_exchange::set_manifest(c, package_id, &manifest_bytes)
                .with_context(|| format!("retain manifest for package {package_id}"))?;
        }
        crate::db::collab_exchange::set_local_status(c, package_id, status)
            .with_context(|| format!("set local status for package {package_id}"))?;
        Ok(())
    })?;

    tracing::info!(
        project_id,
        package_id,
        ingested = ingested_count,
        ok = ok_count,
        rejected = failed.len(),
        status,
        "project ingest done"
    );

    Ok(ProjectIngestOutcome {
        receipts,
        ok_count,
        failed,
    })
}

/// Process one manifest record end to end and return its receipt. Verifies the
/// record (rel_path / payload / xxh3 / project stamp), then supersedes any prior
/// same-uuid contribution and lands the payload, inserting the contribution +
/// history rows in a single transaction. All the fail paths return a `Rejected`
/// receipt inside `Ok`; an `Err` is reserved for an unexpected I/O/DB fault and
/// the caller turns it into a `Rejected` receipt too.
#[allow(clippy::too_many_arguments)]
fn process_project_frame(
    conn: &Connection,
    landing_root: &Path,
    staging_dir: &Path,
    record: &ManifestRecord,
    project_slug: &str,
    publisher: &str,
    project_id: &str,
    package_id: &str,
    peer_device: &str,
    started_at: &str,
) -> Result<FrameReceipt> {
    // Guard the untrusted, wire-supplied rel_path before joining it.
    if let Err(e) = package::validate_rel_path(&record.rel_path) {
        return Ok(rejected_receipt(record, format!("unsafe rel_path: {e}")));
    }
    // Project stamp cross-check FIRST (no disk I/O): the record must be stamped
    // with THIS project and THIS hub package id (audit B1). An unstamped or
    // mis-stamped record — a personal-sync frame, or one lifted from another
    // package — is rejected before we touch its payload or short-circuit as a
    // duplicate.
    let stamp_ok = record
        .project
        .as_ref()
        .is_some_and(|p| p.project_id == project_id && p.package_id == package_id);
    if !stamp_ok {
        tracing::warn!(frame_uuid = %record.frame_uuid, "project ingest stamp mismatch; rejecting");
        return Ok(rejected_receipt(record, "project stamp mismatch".to_string()));
    }

    // uuid-supersede (Д9), duplicate arm resolved BEFORE the payload-presence/hash
    // checks (F1): a byte-identical re-delivery of the same (project, publisher,
    // uuid) is a Duplicate — land nothing. Deciding it without reading the payload
    // lets an already-holding requester complete even against an incomplete serve
    // dir that is missing this overlap payload.
    let existing = existing_contribution_xxh3(conn, project_id, publisher, &record.frame_uuid)?;
    if existing.as_deref() == Some(record.xxh3.as_str()) {
        tracing::debug!(frame_uuid = %record.frame_uuid, "project ingest duplicate (same uuid + xxh3)");
        return Ok(duplicate_receipt(record));
    }

    // Verify integrity: the payload must exist and its full-content xxh3 match.
    let payload = staging_dir.join(&record.rel_path);
    let actual = match package::xxh3_full_file(&payload) {
        Ok(h) => h,
        Err(e) => return Ok(rejected_receipt(record, format!("payload unreadable: {e}"))),
    };
    if actual != record.xxh3 {
        tracing::warn!(frame_uuid = %record.frame_uuid, "project ingest xxh3 mismatch; rejecting");
        return Ok(rejected_receipt(
            record,
            format!("xxh3 mismatch: manifest {}, disk {actual}", record.xxh3),
        ));
    }

    // A different xxh3 for the same (project, publisher, uuid) supersedes the
    // older contribution (its landed file removed best-effort) before the new one
    // lands.
    if let Some(old_path) =
        crate::db::collab_exchange::replace_contribution_for_uuid(conn, project_id, publisher, &record.frame_uuid)?
    {
        if let Err(e) = std::fs::remove_file(&old_path) {
            tracing::warn!(
                path = %old_path,
                error = %e,
                "project ingest: failed to remove superseded contribution file"
            );
        }
    }

    // Land tmp-copy + atomic rename under <root>/<project-slug>/<publisher-slug>/<rel_path>.
    let dest_base = landing_root
        .join(sanitize_slug(project_slug))
        .join(sanitize_slug(publisher))
        .join(Path::new(&record.rel_path));
    let landed = land_project_payload(&dest_base, &payload)
        .with_context(|| format!("land project payload {}", record.rel_path))?;

    // Insert the contribution + history rows in one transaction. If the write
    // fails after the payload landed, remove the orphan so a failed frame never
    // leaves a landed file with no row pointing at it.
    let write: Result<()> = (|| {
        let tx = conn.unchecked_transaction().context("begin contribution tx")?;
        let row = ContributionRow {
            id: 0,
            project_id: project_id.to_string(),
            package_id: package_id.to_string(),
            frame_uuid: record.frame_uuid.clone(),
            publisher_display: publisher.to_string(),
            rel_path: record.rel_path.clone(),
            landed_path: landed.to_string_lossy().to_string(),
            byte_size: record.byte_size as i64,
            xxh3: record.xxh3.clone(),
            frame_meta: serde_json::to_string(&record.frame_meta)
                .unwrap_or_else(|_| "{}".to_string()),
            analysis: record
                .analysis
                .as_ref()
                .and_then(|a| serde_json::to_string(a).ok()),
            superseded: false,
            created_at: String::new(),
        };
        crate::db::collab_exchange::insert_contribution(&tx, &row)
            .context("insert project_contributions row")?;
        insert_history_row(&tx, &received_history(record, peer_device, started_at))
            .context("insert sync_history row")?;
        tx.commit().context("commit contribution tx")
    })();

    if let Err(e) = write {
        if let Err(rm) = std::fs::remove_file(&landed) {
            tracing::warn!(
                path = %landed.display(),
                error = %rm,
                "project ingest: failed to remove orphaned landed file after tx error"
            );
        }
        return Err(e).with_context(|| {
            format!("project ingest write for frame {}", record.frame_uuid)
        });
    }

    tracing::info!(
        frame_uuid = %record.frame_uuid,
        path = %landed.display(),
        "project ingest frame landed"
    );
    Ok(ingested_receipt(record))
}

/// The full-content xxh3 of the current contribution for `(project, publisher,
/// uuid)`, if one is on record. Mirrors [`replace_contribution_for_uuid`]'s WHERE
/// clause so the "same key, same bytes ⇒ Duplicate" decision matches the row the
/// supersede would remove.
fn existing_contribution_xxh3(
    conn: &Connection,
    project_id: &str,
    publisher: &str,
    frame_uuid: &str,
) -> Result<Option<String>> {
    conn.query_row(
        "SELECT xxh3 FROM project_contributions \
         WHERE project_id = ?1 AND publisher_display = ?2 AND frame_uuid = ?3 \
         ORDER BY id LIMIT 1",
        params![project_id, publisher, frame_uuid],
        |r| r.get(0),
    )
    .optional()
    .context("lookup existing contribution xxh3")
}

/// Land an accepted contribution payload at `dest_base` (already the fully
/// resolved `<root>/<project-slug>/<publisher-slug>/<rel_path>`), collision-suffixed
/// via [`unique_path`], with a tmp-copy + atomic rename (mirrors
/// [`super::ingest`]'s `land_payload`). Returns the final path.
fn land_project_payload(dest_base: &Path, payload: &Path) -> Result<PathBuf> {
    let dest = unique_path(dest_base);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create landing dir {}", parent.display()))?;
    }
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

/// Build a `direction = received` history row for a landed contribution. The
/// `object` field carries the manifest's `frame_meta.object` (like personal
/// sync); `project` carries the record's collab [`ProjectStamp`] project id so
/// the Transfers UI can chip the row by project (Task 11).
fn received_history(record: &ManifestRecord, peer_device: &str, started_at: &str) -> HistoryRow {
    HistoryRow {
        frame_uuid: record.frame_uuid.clone(),
        filename: filename_of(&record.rel_path),
        object: record
            .frame_meta
            .get("object")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        peer_device: peer_device.to_string(),
        direction: Direction::Received,
        bytes: record.byte_size,
        started_at: started_at.to_string(),
        finished_at: Some(now_iso()),
        outcome: "ingested".to_string(),
        project: record.project.as_ref().map(|p| p.project_id.clone()),
        // The collab package's durable batch key (the hub package uuid) — the
        // received-detail per-batch key (Task 14) for a project transfer.
        package_id: record.project.as_ref().map(|p| p.package_id.clone()),
        batch_name: None,
    }
}

/// Basename of a forward-slash manifest `rel_path`.
fn filename_of(rel_path: &str) -> String {
    rel_path.rsplit('/').next().unwrap_or(rel_path).to_string()
}

fn ingested_receipt(record: &ManifestRecord) -> FrameReceipt {
    FrameReceipt {
        frame_uuid: record.frame_uuid.clone(),
        xxh3: record.xxh3.clone(),
        outcome: ReceiptOutcome::Ingested,
    }
}
fn duplicate_receipt(record: &ManifestRecord) -> FrameReceipt {
    FrameReceipt {
        frame_uuid: record.frame_uuid.clone(),
        xxh3: record.xxh3.clone(),
        outcome: ReceiptOutcome::Duplicate,
    }
}
fn rejected_receipt(record: &ManifestRecord, reason: String) -> FrameReceipt {
    FrameReceipt {
        frame_uuid: record.frame_uuid.clone(),
        xxh3: record.xxh3.clone(),
        outcome: ReceiptOutcome::Rejected(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::collab::CollabProjectRow;
    use crate::db::collab_exchange::{contributions_for_package, PackageRow};
    use crate::package::{ManifestRecord, PayloadKind, ProjectStamp, MANIFEST_VERSION};
    use tempfile::TempDir;

    const PROJECT_ID: &str = "proj-abc";
    const HUB_PACKAGE_ID: &str = "hub-pkg-1";
    const PUBLISHER: &str = "Alice Nova";
    const PROJECT_SLUG: &str = "orion-mosaic";
    const PEER: &str = "1122334455667788112233445566778811223344556677881122334455667788";

    /// An in-memory catalog with the full schema and FK enforcement.
    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn
    }

    fn seed_project(conn: &Connection) {
        crate::db::collab::upsert_project(
            conn,
            &CollabProjectRow {
                project_id: PROJECT_ID.to_string(),
                slug: PROJECT_SLUG.to_string(),
                title: "Orion Mosaic".to_string(),
                data_role: "contribute".to_string(),
                is_coordinator: false,
                require_approval: false,
                pending_announcements: 0,
                project_status: "active".to_string(),
                target_name: "M42".to_string(),
                target_ra_deg: 83.8,
                target_dec_deg: -5.4,
                target_radius_deg: 1.0,
                membership_version: 1,
                snapshot_payload_b64: "x".to_string(),
                snapshot_signature_b64: "x".to_string(),
                members_json: "[]".to_string(),
                thresholds_version: None,
                thresholds_rules_json: None,
                // local preference — ignored on write
                auto_replicate: true,
                fetched_at: String::new(),
            },
        )
        .unwrap();
    }

    /// Seed the hub-anchored package row with the given manifest anchor.
    fn seed_package(conn: &Connection, manifest_xxh3: Option<&str>) {
        crate::db::collab_exchange::upsert_package(
            conn,
            &PackageRow {
                package_id: HUB_PACKAGE_ID.to_string(),
                project_id: PROJECT_ID.to_string(),
                announcement_id: "ann-1".to_string(),
                publisher_display: PUBLISHER.to_string(),
                own: false,
                root_hash: "roothash".to_string(),
                byte_size: 0,
                frame_count: 0,
                manifest_xxh3: manifest_xxh3.map(|s| s.to_string()),
                aggregate_stats: "{}".to_string(),
                supersedes: "[]".to_string(),
                state: "published".to_string(),
                reject_reason: None,
                superseded: false,
                origin: "remote".to_string(),
                local_dir: None,
                manifest_ndjson: None,
                local_status: "none".to_string(),
                holder_count: 0,
                online_count: 0,
                created_at: "2026-07-13 00:00:00".to_string(),
                decided_at: None,
                fetched_at: String::new(),
            },
        )
        .unwrap();
    }

    /// Seed an additional hub-anchored package row with a custom id (same project
    /// + publisher), modelling an incremental re-publish (F1).
    fn seed_named_package(conn: &Connection, package_id: &str, manifest_xxh3: &str) {
        crate::db::collab_exchange::upsert_package(
            conn,
            &PackageRow {
                package_id: package_id.to_string(),
                project_id: PROJECT_ID.to_string(),
                announcement_id: format!("ann-{package_id}"),
                publisher_display: PUBLISHER.to_string(),
                own: false,
                root_hash: "roothash".to_string(),
                byte_size: 0,
                frame_count: 0,
                manifest_xxh3: Some(manifest_xxh3.to_string()),
                aggregate_stats: "{}".to_string(),
                supersedes: "[]".to_string(),
                state: "published".to_string(),
                reject_reason: None,
                superseded: false,
                origin: "remote".to_string(),
                local_dir: None,
                manifest_ndjson: None,
                local_status: "none".to_string(),
                holder_count: 0,
                online_count: 0,
                created_at: "2026-07-13 00:00:00".to_string(),
                decided_at: None,
                fetched_at: String::new(),
            },
        )
        .unwrap();
    }

    /// Designate a `collaboration` scan root at `path` so landing is deterministic.
    fn seed_collab_root(conn: &Connection, path: &Path) {
        conn.execute(
            "INSERT INTO scan_roots (path, kind) VALUES (?1, 'collaboration')",
            params![path.to_string_lossy().to_string()],
        )
        .unwrap();
    }

    /// Build one manifest record, stamped for THIS project/package unless
    /// overridden. `xxh3_override` lets a test lie about the payload hash.
    fn record(
        frame_uuid: &str,
        rel_path: &str,
        xxh3: &str,
        stamp: Option<ProjectStamp>,
    ) -> ManifestRecord {
        ManifestRecord {
            v: MANIFEST_VERSION,
            frame_uuid: frame_uuid.to_string(),
            origin_catalog_uuid: "cat-uuid".to_string(),
            origin_device: "de".repeat(32),
            payload_kind: PayloadKind::CalibratedLight,
            rel_path: rel_path.to_string(),
            byte_size: 0, // set by stage()
            xxh3: xxh3.to_string(),
            frame_meta: serde_json::json!({ "object": "M42" }),
            analysis: None,
            app_version: "test".to_string(),
            project: stamp,
        }
    }

    fn default_stamp() -> ProjectStamp {
        stamp_for(HUB_PACKAGE_ID)
    }

    /// A project stamp for a specific hub package id (records for a second,
    /// re-published package must be stamped with ITS package id).
    fn stamp_for(package_id: &str) -> ProjectStamp {
        ProjectStamp {
            project_id: PROJECT_ID.to_string(),
            package_id: package_id.to_string(),
            thresholds_version: None,
            cal_engine_version: None,
        }
    }

    /// Write `payloads` (rel_path → bytes) into `staging`, then write a manifest
    /// from `records` (byte_size stamped from disk; each record's `xxh3` is left
    /// exactly as the caller set it, so a lie survives). Returns the manifest
    /// anchor (full-content xxh3 of `manifest.ndjson`).
    fn stage(staging: &Path, mut records: Vec<ManifestRecord>, payloads: &[(&str, &[u8])]) -> String {
        std::fs::create_dir_all(staging).unwrap();
        for (rel, bytes) in payloads {
            let p = staging.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&p, bytes).unwrap();
        }
        for r in &mut records {
            if let Some((_, bytes)) = payloads.iter().find(|(rel, _)| *rel == r.rel_path) {
                r.byte_size = bytes.len() as u64;
            }
        }
        let mut buf = String::new();
        for r in &records {
            buf.push_str(&serde_json::to_string(r).unwrap());
            buf.push('\n');
        }
        let manifest_path = staging.join(package::MANIFEST_FILENAME);
        std::fs::write(&manifest_path, buf.as_bytes()).unwrap();
        package::xxh3_full_file(&manifest_path).unwrap()
    }

    /// Full-content xxh3 of bytes, matching [`package::xxh3_full_file`].
    fn hash_bytes(bytes: &[u8]) -> String {
        format!("{:016x}", xxhash_rust::xxh3::xxh3_64(bytes))
    }

    fn landed_files(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if !root.exists() {
            return out; // nothing landed ⇒ the landing root was never created
        }
        for e in walkdir::WalkDir::new(root) {
            let e = e.unwrap();
            if e.file_type().is_file() {
                out.push(e.path().to_path_buf());
            }
        }
        out
    }

    // 1. Manifest anchor mismatch rejects the whole package (hard error, nothing lands).
    #[test]
    fn anchor_mismatch_rejects_everything() {
        let tmp = TempDir::new().unwrap();
        let conn = test_conn();
        let landing = tmp.path().join("landing");
        seed_project(&conn);
        seed_collab_root(&conn, &landing);

        let staging = tmp.path().join("staging");
        let bytes = b"frame-content-A";
        let rec = record("u-1", "L_0001.fits", &hash_bytes(bytes), Some(default_stamp()));
        let anchor = stage(&staging, vec![rec], &[("L_0001.fits", bytes)]);

        // Seed the package with a DIFFERENT anchor than the staged manifest.
        assert_ne!(anchor, "deadbeefdeadbeef");
        seed_package(&conn, Some("deadbeefdeadbeef"));

        let err = ingest_project_package(IngestConn::Borrowed(&conn), &staging, PROJECT_ID, HUB_PACKAGE_ID, PEER)
            .expect_err("anchor mismatch must be a hard error");
        let msg = format!("{err:#}");
        assert!(msg.contains("manifest anchor mismatch"), "names the failure: {msg}");
        assert!(msg.contains(&anchor), "names the staged hash");

        assert!(contributions_for_package(&conn, HUB_PACKAGE_ID).unwrap().is_empty());
        assert!(landed_files(&landing).is_empty(), "nothing landed on anchor mismatch");
    }

    // NULL anchor is a hard error with the exact spec message.
    #[test]
    fn null_anchor_is_hard_error() {
        let tmp = TempDir::new().unwrap();
        let conn = test_conn();
        seed_project(&conn);
        seed_collab_root(&conn, &tmp.path().join("landing"));
        seed_package(&conn, None);

        let staging = tmp.path().join("staging");
        let bytes = b"x";
        let rec = record("u-1", "a.fits", &hash_bytes(bytes), Some(default_stamp()));
        stage(&staging, vec![rec], &[("a.fits", bytes)]);

        let err = ingest_project_package(IngestConn::Borrowed(&conn), &staging, PROJECT_ID, HUB_PACKAGE_ID, PEER)
            .expect_err("null anchor must be a hard error");
        assert!(format!("{err:#}").contains("announcement carries no manifest anchor"));
    }

    // A hub-rejected package is refused wholesale before any frame lands (I2) — a
    // rejected contributor must not be able to re-push the rejected content.
    #[test]
    fn rejected_state_is_hard_error() {
        let tmp = TempDir::new().unwrap();
        let conn = test_conn();
        let landing = tmp.path().join("landing");
        seed_project(&conn);
        seed_collab_root(&conn, &landing);

        let staging = tmp.path().join("staging");
        let bytes = b"rejected-content";
        let rec = record("u-1", "a.fits", &hash_bytes(bytes), Some(default_stamp()));
        let anchor = stage(&staging, vec![rec], &[("a.fits", bytes)]);
        seed_package(&conn, Some(&anchor));
        // Flip the hub-mirrored state to rejected (as a coordinator's poll would).
        let mut row = crate::db::collab_exchange::get_package(&conn, HUB_PACKAGE_ID)
            .unwrap()
            .unwrap();
        row.state = "rejected".to_string();
        crate::db::collab_exchange::upsert_package(&conn, &row).unwrap();

        let err = ingest_project_package(IngestConn::Borrowed(&conn), &staging, PROJECT_ID, HUB_PACKAGE_ID, PEER)
            .expect_err("a rejected package must be refused");
        assert!(
            format!("{err:#}").contains("rejected"),
            "the error names the state: {err:#}"
        );
        assert!(contributions_for_package(&conn, HUB_PACKAGE_ID).unwrap().is_empty());
        assert!(landed_files(&landing).is_empty(), "nothing landed for a rejected package");
    }

    // 2. A bad per-frame xxh3 ⇒ Rejected, while good frames still land.
    #[test]
    fn bad_payload_xxh3_rejected_good_frames_land() {
        let tmp = TempDir::new().unwrap();
        let conn = test_conn();
        let landing = tmp.path().join("landing");
        seed_project(&conn);
        seed_collab_root(&conn, &landing);

        let staging = tmp.path().join("staging");
        let good = b"good-frame-bytes";
        let bad = b"bad-frame-bytes";
        let good_rec = record("u-good", "good.fits", &hash_bytes(good), Some(default_stamp()));
        // A lie: the record claims a hash the payload does not have.
        let bad_rec = record("u-bad", "bad.fits", "0000000000000000", Some(default_stamp()));
        let anchor = stage(
            &staging,
            vec![good_rec, bad_rec],
            &[("good.fits", good), ("bad.fits", bad)],
        );
        seed_package(&conn, Some(&anchor));

        let out =
            ingest_project_package(IngestConn::Borrowed(&conn), &staging, PROJECT_ID, HUB_PACKAGE_ID, PEER).unwrap();
        assert_eq!(out.ok_count, 1);
        assert_eq!(out.failed, vec!["u-bad".to_string()]);
        let by_uuid = |u: &str| out.receipts.iter().find(|r| r.frame_uuid == u).unwrap();
        assert!(matches!(by_uuid("u-good").outcome, ReceiptOutcome::Ingested));
        assert!(matches!(by_uuid("u-bad").outcome, ReceiptOutcome::Rejected(_)));

        // Only the good frame landed and got a contribution row.
        let rows = contributions_for_package(&conn, HUB_PACKAGE_ID).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].frame_uuid, "u-good");
        let files = landed_files(&landing);
        assert_eq!(files.len(), 1, "only the good frame landed");
        // A partial failure marks the package failed, but the manifest is retained.
        assert_eq!(
            crate::db::collab_exchange::get_package(&conn, HUB_PACKAGE_ID)
                .unwrap()
                .unwrap()
                .local_status,
            "failed"
        );
    }

    // 3. Landing path shape: <root>/<project-slug>/<publisher-slug>/<rel_path>.
    #[test]
    fn landing_path_shape_is_hub_anchored() {
        let tmp = TempDir::new().unwrap();
        let conn = test_conn();
        let landing = tmp.path().join("landing");
        seed_project(&conn);
        seed_collab_root(&conn, &landing);

        let staging = tmp.path().join("staging");
        let bytes = b"payload";
        let rec = record("u-1", "sub/L_0001.fits", &hash_bytes(bytes), Some(default_stamp()));
        let anchor = stage(&staging, vec![rec], &[("sub/L_0001.fits", bytes)]);
        seed_package(&conn, Some(&anchor));

        ingest_project_package(IngestConn::Borrowed(&conn), &staging, PROJECT_ID, HUB_PACKAGE_ID, PEER).unwrap();

        let rows = contributions_for_package(&conn, HUB_PACKAGE_ID).unwrap();
        assert_eq!(rows.len(), 1);
        let landed = PathBuf::from(&rows[0].landed_path);
        // publisher slug is sanitized from the HUB publisher_display, not the peer.
        let expected = landing
            .join(sanitize_slug(PROJECT_SLUG))
            .join(sanitize_slug(PUBLISHER))
            .join("sub")
            .join("L_0001.fits");
        assert_eq!(landed, expected, "landed under <root>/<proj-slug>/<pub-slug>/<rel>");
        assert!(landed.exists());
        assert_eq!(
            crate::db::collab_exchange::get_package(&conn, HUB_PACKAGE_ID)
                .unwrap()
                .unwrap()
                .local_status,
            "complete"
        );
    }

    // 4. uuid re-publication (new content) replaces the row AND removes the old file.
    #[test]
    fn uuid_republish_supersedes_row_and_old_file() {
        let tmp = TempDir::new().unwrap();
        let conn = test_conn();
        let landing = tmp.path().join("landing");
        seed_project(&conn);
        seed_collab_root(&conn, &landing);

        // First delivery: content v1.
        let staging1 = tmp.path().join("staging1");
        let v1 = b"content-version-1";
        let rec1 = record("u-1", "L.fits", &hash_bytes(v1), Some(default_stamp()));
        let anchor1 = stage(&staging1, vec![rec1], &[("L.fits", v1)]);
        seed_package(&conn, Some(&anchor1));
        ingest_project_package(IngestConn::Borrowed(&conn), &staging1, PROJECT_ID, HUB_PACKAGE_ID, PEER).unwrap();

        let first = contributions_for_package(&conn, HUB_PACKAGE_ID).unwrap();
        assert_eq!(first.len(), 1);
        let old_path = PathBuf::from(&first[0].landed_path);
        assert!(old_path.exists());

        // Re-publication: SAME uuid, DIFFERENT content (v2). New anchor.
        let staging2 = tmp.path().join("staging2");
        let v2 = b"content-version-2-different";
        let rec2 = record("u-1", "L.fits", &hash_bytes(v2), Some(default_stamp()));
        let anchor2 = stage(&staging2, vec![rec2], &[("L.fits", v2)]);
        crate::db::collab_exchange::upsert_package(
            &conn,
            &PackageRow {
                manifest_xxh3: Some(anchor2.clone()),
                ..crate::db::collab_exchange::get_package(&conn, HUB_PACKAGE_ID)
                    .unwrap()
                    .unwrap()
            },
        )
        .unwrap();
        let out =
            ingest_project_package(IngestConn::Borrowed(&conn), &staging2, PROJECT_ID, HUB_PACKAGE_ID, PEER).unwrap();
        assert!(matches!(out.receipts[0].outcome, ReceiptOutcome::Ingested));

        // Exactly one contribution row (the old was superseded), pointing at v2.
        let rows = contributions_for_package(&conn, HUB_PACKAGE_ID).unwrap();
        assert_eq!(rows.len(), 1, "old row superseded, one row remains");
        assert_eq!(rows[0].xxh3, hash_bytes(v2));
        // The re-published frame reuses the same rel_path: because the old file
        // was removed first, `unique_path` finds the base free and lands v2 at
        // the SAME path (no `_2` suffix). So "old file removed" is observable as
        // exactly ONE file on disk whose content is now v2 — had the old file
        // survived, the new one would have collision-suffixed to a second file.
        let new_path = PathBuf::from(&rows[0].landed_path);
        assert_eq!(new_path, old_path, "v2 reuses the freed rel_path");
        assert_eq!(package::xxh3_full_file(&new_path).unwrap(), hash_bytes(v2), "content is v2");
        assert_eq!(landed_files(&landing).len(), 1, "old file removed ⇒ one file on disk");
    }

    // 5. Byte-identical re-delivery ⇒ Duplicate, no second file, one row.
    #[test]
    fn identical_redelivery_is_duplicate() {
        let tmp = TempDir::new().unwrap();
        let conn = test_conn();
        let landing = tmp.path().join("landing");
        seed_project(&conn);
        seed_collab_root(&conn, &landing);

        let staging = tmp.path().join("staging");
        let bytes = b"same-bytes-every-time";
        let mk = || record("u-1", "L.fits", &hash_bytes(bytes), Some(default_stamp()));
        let anchor = stage(&staging, vec![mk()], &[("L.fits", bytes)]);
        seed_package(&conn, Some(&anchor));

        // First delivery ingests.
        let out1 =
            ingest_project_package(IngestConn::Borrowed(&conn), &staging, PROJECT_ID, HUB_PACKAGE_ID, PEER).unwrap();
        assert!(matches!(out1.receipts[0].outcome, ReceiptOutcome::Ingested));
        assert_eq!(landed_files(&landing).len(), 1);

        // Second delivery of the exact same package ⇒ Duplicate, nothing new.
        let out2 =
            ingest_project_package(IngestConn::Borrowed(&conn), &staging, PROJECT_ID, HUB_PACKAGE_ID, PEER).unwrap();
        assert!(matches!(out2.receipts[0].outcome, ReceiptOutcome::Duplicate));
        assert_eq!(out2.ok_count, 1);
        assert!(out2.failed.is_empty());
        assert_eq!(
            contributions_for_package(&conn, HUB_PACKAGE_ID).unwrap().len(),
            1,
            "no second row"
        );
        assert_eq!(landed_files(&landing).len(), 1, "no second file");
    }

    // 6. A record stamped for a different project (or package) ⇒ Rejected.
    #[test]
    fn stamp_mismatch_rejected() {
        let tmp = TempDir::new().unwrap();
        let conn = test_conn();
        let landing = tmp.path().join("landing");
        seed_project(&conn);
        seed_collab_root(&conn, &landing);

        let staging = tmp.path().join("staging");
        let bytes = b"payload";
        // Wrong project_id in the stamp.
        let wrong = ProjectStamp {
            project_id: "some-other-project".to_string(),
            package_id: HUB_PACKAGE_ID.to_string(),
            thresholds_version: None,
            cal_engine_version: None,
        };
        let rec = record("u-1", "L.fits", &hash_bytes(bytes), Some(wrong));
        let anchor = stage(&staging, vec![rec], &[("L.fits", bytes)]);
        seed_package(&conn, Some(&anchor));

        let out =
            ingest_project_package(IngestConn::Borrowed(&conn), &staging, PROJECT_ID, HUB_PACKAGE_ID, PEER).unwrap();
        assert!(matches!(out.receipts[0].outcome, ReceiptOutcome::Rejected(_)));
        assert_eq!(out.ok_count, 0);
        assert_eq!(out.failed, vec!["u-1".to_string()]);
        assert!(contributions_for_package(&conn, HUB_PACKAGE_ID).unwrap().is_empty());
        assert!(landed_files(&landing).is_empty());
        // Nothing ingested ⇒ manifest not retained, status failed.
        let pkg = crate::db::collab_exchange::get_package(&conn, HUB_PACKAGE_ID)
            .unwrap()
            .unwrap();
        assert!(pkg.manifest_ndjson.is_none());
        assert_eq!(pkg.local_status, "failed");
    }

    // Fallback landing root when no `collaboration` scan root is designated.
    #[test]
    fn fallback_landing_root_under_sync_dir() {
        let tmp = TempDir::new().unwrap();
        let conn = test_conn();
        seed_project(&conn);
        // No collaboration scan root seeded.

        // Mirror the receiver's staging layout: <sync_dir>/staging/<wire_id>.
        let sync_dir = tmp.path().join("sync");
        let staging = sync_dir.join("staging").join("wire-pkg-xyz");
        let bytes = b"payload";
        let rec = record("u-1", "L.fits", &hash_bytes(bytes), Some(default_stamp()));
        let anchor = stage(&staging, vec![rec], &[("L.fits", bytes)]);
        seed_package(&conn, Some(&anchor));

        ingest_project_package(IngestConn::Borrowed(&conn), &staging, PROJECT_ID, HUB_PACKAGE_ID, PEER).unwrap();

        let rows = contributions_for_package(&conn, HUB_PACKAGE_ID).unwrap();
        assert_eq!(rows.len(), 1);
        let expected_root = sync_dir.join("collaboration");
        assert!(
            PathBuf::from(&rows[0].landed_path).starts_with(&expected_root),
            "fallback lands under <sync_dir>/collaboration: {}",
            rows[0].landed_path
        );
    }

    // F1/C1: an incremental re-publish re-includes unchanged frames, which ingest
    // as Duplicates and keep their contribution row keyed to the ORIGINAL package.
    // `reconstruct_serve_dir` must still serve the COMPLETE re-published package by
    // resolving payloads from the retained manifest BY CONTENT HASH, not the
    // package key. ("collab" in the name so the focused `--lib collab` gate runs it.)
    #[test]
    fn collab_incremental_republish_reconstructs_complete_serve() {
        let tmp = TempDir::new().unwrap();
        let conn = test_conn();
        let landing = tmp.path().join("landing");
        seed_project(&conn);
        seed_collab_root(&conn, &landing);

        // ── pkg1 (HUB_PACKAGE_ID): frame A (shared) + B (v1). ─────────────────
        let a = b"frame-A-shared-content";
        let b1 = b"frame-B-content-v1";
        let staging1 = tmp.path().join("staging1");
        let anchor1 = stage(
            &staging1,
            vec![
                record("u-A", "A.fits", &hash_bytes(a), Some(default_stamp())),
                record("u-B", "B.fits", &hash_bytes(b1), Some(default_stamp())),
            ],
            &[("A.fits", a), ("B.fits", b1)],
        );
        seed_package(&conn, Some(&anchor1));
        let out1 =
            ingest_project_package(IngestConn::Borrowed(&conn), &staging1, PROJECT_ID, HUB_PACKAGE_ID, PEER).unwrap();
        assert_eq!(out1.ok_count, 2);

        // ── pkg2: A re-included unchanged (Duplicate), B changed (v2), C new. ─
        const PKG2: &str = "hub-pkg-2";
        let b2 = b"frame-B-content-v2-different";
        let c = b"frame-C-brand-new";
        let staging2 = tmp.path().join("staging2");
        let anchor2 = stage(
            &staging2,
            vec![
                record("u-A", "A.fits", &hash_bytes(a), Some(stamp_for(PKG2))),
                record("u-B", "B.fits", &hash_bytes(b2), Some(stamp_for(PKG2))),
                record("u-C", "C.fits", &hash_bytes(c), Some(stamp_for(PKG2))),
            ],
            &[("A.fits", a), ("B.fits", b2), ("C.fits", c)],
        );
        seed_named_package(&conn, PKG2, &anchor2);
        let out2 = ingest_project_package(IngestConn::Borrowed(&conn), &staging2, PROJECT_ID, PKG2, PEER).unwrap();

        // A is a byte-identical re-delivery ⇒ Duplicate; B(v2) + C ingest.
        let by_uuid = |u: &str| out2.receipts.iter().find(|r| r.frame_uuid == u).unwrap();
        assert!(matches!(by_uuid("u-A").outcome, ReceiptOutcome::Duplicate));
        assert!(matches!(by_uuid("u-B").outcome, ReceiptOutcome::Ingested));
        assert!(matches!(by_uuid("u-C").outcome, ReceiptOutcome::Ingested));

        // The overlap frame A's contribution row stayed keyed to the ORIGINAL
        // package — a PKG2-keyed lookup misses it (the bug's root cause).
        assert_eq!(
            contributions_for_package(&conn, PKG2).unwrap().len(),
            2,
            "only B(v2) + C are keyed to PKG2; A stayed on pkg1"
        );

        // Reconstruct + validate the PKG2 serve dir: manifest-driven, hash-resolved,
        // COMPLETE. `validate_package` re-hashes every manifest payload end to end.
        let sync_dir = tmp.path().join("sync");
        let serve_dir =
            crate::api::collab_exchange::reconstruct_serve_dir(&conn, &sync_dir, PKG2).unwrap();
        crate::package::validate_package(&serve_dir).unwrap();
        for name in ["A.fits", "B.fits", "C.fits"] {
            assert!(serve_dir.join(name).exists(), "{name} served in the reconstructed dir");
        }
    }
}
