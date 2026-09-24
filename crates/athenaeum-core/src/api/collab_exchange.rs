//! Holder-side collaboration package exchange — request-to-serve (Stage II
//! collaboration, slice 4, task 6).
//!
//! When a project member asks us (a holder) to serve a package
//! ([`TransportEvent::ProjectRequestReceived`](crate::sharing::types::TransportEvent),
//! wired at the receiver in [`crate::api::sync`]), this module answers:
//!
//! 1. [`handle_project_request`] authorizes the requester
//!    ([`may_serve_package`](crate::collab::authz::may_serve_package) — a
//!    published package to any `send_receive` member or the coordinator, a still
//!    pending one only to the coordinator, Д1), reconstructs a servable directory,
//!    and enqueues an explicit-target send back to the requester through a
//!    DEDICATED collab sender map. Any authorization failure is a **silent**
//!    (warn-logged) drop — cross-account, no error is sent on the wire.
//! 2. [`reconstruct_serve_dir`] rebuilds a byte-identical package directory: a
//!    package I published (`origin='mine'`) returns its retained publication dir
//!    as-is; one I fully received (`origin='received'`) is materialized under
//!    `<sync_dir>/collab_serve/<package_id>/` from the retained manifest bytes +
//!    the landed contribution payloads (hard-linked, copy fallback), idempotently.
//! 3. [`CollabCleanupSink`] cleans a reconstructed `collab_serve` dir once the
//!    serve reaches a terminal state, while NEVER touching a retained
//!    `collab_pub` publication (Д4).
//! 4. [`ensure_collab_sender_engine`] mirrors [`crate::api::sync::ensure_sender_engine`]
//!    but binds a dedicated `<sync_dir>/blobs_collab` blob store (audit m7: a
//!    second `FsStore` over the personal-sync `blobs_out` risks the redb lock and
//!    the startup tag-sweep) and spawns each engine with the [`CollabCleanupSink`]
//!    + a host emitter so a project serve still surfaces `sync-progress` /
//!    `sync-finished`.
//!
//! Ungated (no render gate): depends only on `db`, `sync`, `sharing`, `collab`,
//! `package`, so it compiles in the headless (`--no-default-features`) build.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};

use crate::account::keys::{device_key_path, DeviceKey};
use crate::api::{db, ApiError};
use crate::collab::hub_client::{AnnouncementWire, CollabClient, HolderWire};
use crate::collab::snapshot::{own_display_name, SnapshotMember};
use crate::db::collab_exchange::{
    contributions_for_project, get_package, list_packages, mark_superseded, set_local_status,
    upsert_package, ContributionRow, PackageRow,
};
use crate::db::collab_frames::{FrameOrigin, LocalFrameRow};
use crate::events::ProgressEmitter;
use crate::export::models::WbppExportConfig;
use crate::services::ServiceContext;
use crate::sharing::iroh::node::Role;
use crate::sharing::types::{NodeId, PackageId, PackageLayout};
use crate::sharing::{noop_fetch_sink, ProviderEvent, ProviderTelemetrySink, SharingTransport};
use crate::sync::{
    node_id_hex, pairing, CatalogSyncStore, PackageCleanupSink, StartedSender, SyncEngine,
    SyncEngineHandle, SyncSenderRuntime, SyncStore,
};

/// Rebuild a servable package directory for a locally-held package (`package_id`
/// is the HUB package uuid — the `project_packages` row key).
///
/// - `origin='mine'` ⇒ returns the retained publication dir (`local_dir`) as-is;
///   it already holds a byte-identical manifest + payloads.
/// - `origin='received'` (or any other locally-held origin) ⇒ materializes
///   `<sync_dir>/collab_serve/<package_id>/`: `manifest.ndjson` is written
///   byte-exact from the retained `manifest_ndjson`, and each MANIFEST RECORD's
///   payload is resolved among locally-held contributions by content hash
///   (`find_contribution_by_project_and_hash`, scoped to the package's project)
///   and hard-linked to its `rel_path` (a byte copy is the fallback when
///   hard-linking is impossible, e.g. a cross-device landing root). Resolving by
///   hash — not by package key — is load-bearing (C1/F1): an incremental
///   re-publish re-includes unchanged frames whose contribution rows stay keyed
///   to the ORIGINAL package, so a package-keyed lookup would serve an INCOMPLETE
///   dir. A manifest record with no locally-held payload is a HARD error (we
///   refuse to serve a package we cannot fully reconstruct). Idempotent — a
///   second call re-writes the manifest and skips payloads already present.
pub fn reconstruct_serve_dir(
    conn: &rusqlite::Connection,
    sync_dir: &Path,
    package_id: &str,
) -> Result<PathBuf> {
    let pkg = crate::db::collab_exchange::get_package(conn, package_id)?
        .ok_or_else(|| anyhow!("collab package {package_id} not found for serving"))?;

    // A package I published: the retained publication dir is servable as-is (Д2).
    if pkg.origin == "mine" {
        let dir = pkg
            .local_dir
            .as_deref()
            .ok_or_else(|| anyhow!("own collab package {package_id} has no retained local_dir"))?;
        return Ok(PathBuf::from(dir));
    }

    // A package I received: materialize a fresh serve dir from the retained
    // manifest bytes + landed contributions. Guard the hub id (ultimately
    // peer-minted, even if it survived a prior validate_package_id) before it is
    // used as a path segment (C1).
    crate::package::validate_package_id(package_id)
        .with_context(|| format!("reject unsafe collab package id {package_id}"))?;
    let manifest_bytes = pkg.manifest_ndjson.as_deref().ok_or_else(|| {
        anyhow!("collab package {package_id} has no retained manifest to re-serve")
    })?;
    let records = parse_manifest_bytes(manifest_bytes)
        .with_context(|| format!("parse retained manifest for {package_id}"))?;
    let serve_dir = sync_dir.join("collab_serve").join(package_id);
    materialize_package_dir(
        conn,
        &pkg.project_id,
        package_id,
        &serve_dir,
        manifest_bytes,
        &records,
    )
    .with_context(|| format!("reconstruct collab serve dir for {package_id}"))?;
    tracing::info!(
        package_id,
        count = records.len(),
        path = %serve_dir.display(),
        "collab serve dir reconstructed"
    );
    Ok(serve_dir)
}

/// Where a RECEIVED package's seed dir lives: `<sync_dir>/collab_seed/<package_id>`
/// (D3 §3.4). Deliberately NOT `collab_serve`, and that separation is the whole
/// point — see [`reconstruct_seed_dir`].
const SEED_DIR: &str = "collab_seed";

/// Rebuild the directory this device SEEDS for a locally-held package — the
/// lifetime-safe twin of [`reconstruct_serve_dir`] (D3 §3.4).
///
/// - `origin='mine'` ⇒ the retained publication dir (`local_dir`), which publish
///   already seeded (D3 T2) and which only its own supersede-reclaim deletes.
/// - anything received ⇒ `<sync_dir>/collab_seed/<package_id>`, materialized from
///   the retained manifest + the landed contributions by the SAME
///   [`materialize_package_dir`] the serve path uses (hard links, byte-copy
///   fallback), and idempotent the same way.
///
/// **Why not just seed the serve dir.** A seed is imported with
/// [`ImportMode::TryReference`](iroh_blobs::api::blobs::ImportMode::TryReference),
/// which makes the blob store keep a REFERENCE TO A PATH (vendored
/// `store/fs/import.rs` → `ImportSource::External(path, …)`), re-opened on every
/// later read. A `collab_serve/<pkg>` dir is temporary by contract:
/// [`CollabCleanupSink::on_terminal`] deletes it at every push-serve terminal.
/// Seeding it would therefore leave a permanently-tagged collection whose blobs
/// point at paths the next serve deletes — a device the hub advertises as a
/// holder but that fails every GET. `collab_seed/<pkg>` is touched by nothing
/// except the unseed paths ([`unseed_package_local_data`] /
/// [`unseed_project_local_data`]), and it is not the swarm staging dir
/// ([`SWARM_STAGING_DIR`], removed after every fetch) either.
///
/// When the store ALREADY holds the content — the common case right after a
/// fetch — the import is satisfied from what is there and no reference is taken;
/// the dir is what a re-import reads when the store does not (measured both ways
/// by `downloader_seed_survives_collab_serve_cleanup` and the three-node e2e). It
/// must therefore outlive the tag in either case, which is why every unseed site
/// drops the tag and this dir together.
///
/// **Why hard links are the right reference.** The seed dir's entries are hard
/// links to the landed contribution files, so the store's referenced path stays
/// valid for as long as the SEED itself does — even when the contribution is
/// later superseded and its landed file unlinked (`replace_contribution_for_uuid`
/// + the caller's `remove_file`), the inode survives behind our link and the seed
/// keeps serving exactly the bytes its collection hash names. Referencing
/// `landed_path` directly would have made that ordinary re-publish silently
/// break every older package this device seeds. On a landing root that cannot be
/// hard-linked (a different volume), the fallback is a byte copy — the same
/// trade the serve path already makes.
pub fn reconstruct_seed_dir(
    conn: &rusqlite::Connection,
    sync_dir: &Path,
    package_id: &str,
) -> Result<PathBuf> {
    let pkg = crate::db::collab_exchange::get_package(conn, package_id)?
        .ok_or_else(|| anyhow!("collab package {package_id} not found for seeding"))?;

    if pkg.origin == "mine" {
        let dir = pkg
            .local_dir
            .as_deref()
            .ok_or_else(|| anyhow!("own collab package {package_id} has no retained local_dir"))?;
        return Ok(PathBuf::from(dir));
    }

    crate::package::validate_package_id(package_id)
        .with_context(|| format!("reject unsafe collab package id {package_id}"))?;
    let manifest_bytes = pkg
        .manifest_ndjson
        .as_deref()
        .ok_or_else(|| anyhow!("collab package {package_id} has no retained manifest to seed"))?;
    let records = parse_manifest_bytes(manifest_bytes)
        .with_context(|| format!("parse retained manifest for {package_id}"))?;
    let seed_dir = sync_dir.join(SEED_DIR).join(package_id);
    materialize_package_dir(
        conn,
        &pkg.project_id,
        package_id,
        &seed_dir,
        manifest_bytes,
        &records,
    )
    .with_context(|| format!("materialize collab seed dir for {package_id}"))?;
    tracing::debug!(
        package_id,
        count = records.len(),
        path = %seed_dir.display(),
        "collab seed dir materialized"
    );
    Ok(seed_dir)
}

/// Parse retained NDJSON manifest bytes into records (blank lines skipped,
/// unknown JSON keys ignored — mirrors [`crate::package::read_manifest`], but
/// from bytes we already hold rather than re-reading a file).
fn parse_manifest_bytes(bytes: &[u8]) -> Result<Vec<crate::package::ManifestRecord>> {
    let text = std::str::from_utf8(bytes).context("retained manifest is not valid utf-8")?;
    let mut records = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record = serde_json::from_str(line)
            .with_context(|| format!("parse retained manifest line {}", i + 1))?;
        records.push(record);
    }
    Ok(records)
}

/// Every manifest record's payload resolves to a locally-held contribution by
/// `(project_id, xxh3)` — the completeness predicate the have-report gate (F1)
/// uses before advertising a package to the hub. The read-only twin of
/// [`materialize_serve_dir`]'s per-record content-hash resolution.
fn manifest_fully_local(
    conn: &rusqlite::Connection,
    project_id: &str,
    records: &[crate::package::ManifestRecord],
) -> Result<bool> {
    for record in records {
        if crate::db::collab_exchange::find_contribution_by_project_and_hash(
            conn,
            project_id,
            &record.xxh3,
        )?
        .is_none()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Outcome of [`package_fully_held`]. `NotComplete` and `NoManifest` are the
/// ORDINARY course of a package still in flight or never retained; only
/// `ManifestNotCovered` — locally `complete` yet missing payload the retained
/// manifest promises — is the anomaly worth a `warn!` at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeldState {
    /// `local_status == "complete"` and every manifest record resolves to a
    /// locally-held contribution — this device can actually serve it.
    Held,
    /// `local_status != "complete"` — still downloading, failed, or never
    /// started. Ordinary; not logged above `debug!`.
    NotComplete,
    /// `local_status == "complete"` but no manifest was ever retained
    /// (`manifest_ndjson` is `NULL`). Ordinary for a row that pre-dates
    /// manifest retention or was never fully received; not logged above
    /// `debug!`.
    NoManifest,
    /// `local_status == "complete"` AND a manifest is retained, but
    /// [`manifest_fully_local`] found a record with no locally-held payload.
    /// This SHOULD NOT happen — it means the row claims completeness the
    /// content doesn't back up — so the call site warns.
    ManifestNotCovered,
}

/// The have-report completeness gate (F1): local status is `complete` AND the
/// retained manifest's every record resolves to a locally-held contribution
/// ([`manifest_fully_local`]) — the same predicate [`reconstruct_serve_dir`]
/// enforces before it will serve a package. Shared by the post-ingest hook
/// ([`report_have_after_ingest`]) and the auto-sync pass's full-set report
/// ([`held_package_ids_for_project`]), so both agree on what "held" means.
/// Returns a [`HeldState`] rather than a bare bool so callers can tell the
/// ordinary "not held yet" cases from the complete-but-uncovered anomaly and
/// log each at its own level — one predicate, two log levels.
fn package_fully_held(conn: &rusqlite::Connection, row: &PackageRow) -> Result<HeldState> {
    if row.local_status != "complete" {
        return Ok(HeldState::NotComplete);
    }
    let Some(bytes) = row.manifest_ndjson.as_deref() else {
        return Ok(HeldState::NoManifest);
    };
    let records = parse_manifest_bytes(bytes)?;
    if manifest_fully_local(conn, &row.project_id, &records)? {
        Ok(HeldState::Held)
    } else {
        Ok(HeldState::ManifestNotCovered)
    }
}

/// Every package of `project_id` this device currently fully holds
/// ([`package_fully_held`] == [`HeldState::Held`]) — the body of the
/// auto-sync pass's `PUT /projects/{id}/have` re-confirmation.
#[allow(dead_code)] // the package layer retires in Task 12
pub(crate) fn held_package_ids_for_project(
    conn: &rusqlite::Connection,
    project_id: &str,
) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for row in list_packages(conn, project_id)? {
        if package_fully_held(conn, &row)? == HeldState::Held {
            ids.push(row.package_id);
        }
    }
    Ok(ids)
}

/// Materialize `dest_dir` from the RETAINED MANIFEST (Д2): `manifest.ndjson`
/// byte-exact from `manifest_bytes`, then for each manifest record resolve its
/// payload among locally-held contributions by content hash
/// (`find_contribution_by_project_and_hash`, scoped to `project_id`) and
/// hard-link it at the record's `rel_path`.
///
/// Resolving by CONTENT HASH — not the package key — is the fix for C1/F1: an
/// incremental re-publish re-includes unchanged frames, whose contribution rows
/// stay keyed to the ORIGINAL package (the receiver returns `Duplicate` and
/// writes no new row for them). A package-keyed lookup would miss every such
/// overlap payload and serve an INCOMPLETE package; the hash lookup finds them
/// wherever they landed. Any manifest record with NO locally-held payload is a
/// HARD error — we refuse to serve a package we cannot fully reconstruct
/// (the request handler's silent-drop discipline turns the error into a refusal).
/// Idempotent — a second call re-writes the manifest and skips present payloads.
///
/// Two callers, two destinations: [`reconstruct_serve_dir`] (the temporary
/// `collab_serve/<pkg>` a push-serve enqueues) and [`reconstruct_seed_dir`] (the
/// durable `collab_seed/<pkg>` a seed references). Both want byte-identical
/// content laid out at the manifest's `rel_path`s; only their lifetimes differ.
fn materialize_package_dir(
    conn: &rusqlite::Connection,
    project_id: &str,
    package_id: &str,
    dest_dir: &Path,
    manifest_bytes: &[u8],
    records: &[crate::package::ManifestRecord],
) -> Result<()> {
    std::fs::create_dir_all(dest_dir)
        .with_context(|| format!("create collab package dir {}", dest_dir.display()))?;
    // The manifest is written byte-exact so a re-serve is byte-identical to the
    // original (Д2). Overwriting with the same bytes keeps the second call idempotent.
    let manifest_path = dest_dir.join(crate::package::MANIFEST_FILENAME);
    std::fs::write(&manifest_path, manifest_bytes)
        .with_context(|| format!("write serve manifest {}", manifest_path.display()))?;

    for record in records {
        // `rel_path` originated in a peer's manifest — guard before joining (L1).
        crate::package::validate_rel_path(&record.rel_path)
            .with_context(|| format!("reject unsafe manifest rel_path {}", record.rel_path))?;
        let dest = dest_dir.join(&record.rel_path);
        if dest.exists() {
            // Idempotent second call: the payload is already materialized.
            continue;
        }
        // Resolve the payload by content hash among ALL of this project's
        // contributions — an incremental re-publish's overlap frames keep their
        // row keyed to the original package, so a package-keyed lookup would miss
        // them (C1/F1). No local payload ⇒ refuse to serve an incomplete package.
        let contribution = crate::db::collab_exchange::find_contribution_by_project_and_hash(
            conn,
            project_id,
            &record.xxh3,
        )?
        .ok_or_else(|| {
            anyhow!(
                "cannot re-serve package: no local payload for rel_path {} (xxh3 {})",
                record.rel_path,
                record.xxh3
            )
        })?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create serve payload dir {}", parent.display()))?;
        }
        let src = Path::new(&contribution.landed_path);
        // Hard-link the landed copy into the destination — no second full copy of
        // the frame. Fall back to a byte copy when hard-linking is impossible (a
        // landing root on a different volume from the sync dir, EXDEV, or any
        // other link error).
        //
        // WARN, not debug (F3): for a `collab_seed/<pkg>` dir the copy is
        // PERMANENT — the seed lives as long as the package does — so a
        // cross-volume landing root silently doubles the on-disk cost of every
        // package this device seeds. The serve dir's copy is transient by
        // comparison (`CollabCleanupSink` takes it at the next terminal), but one
        // honest line per materialized package is worth it either way. Storage
        // accounting for the duplicate is a named follow-up, not this log line.
        if let Err(e) = std::fs::hard_link(src, &dest) {
            tracing::warn!(
                package_id,
                src = %src.display(),
                dest = %dest.display(),
                bytes = record.byte_size,
                error = %e,
                "collab package dir: hard link failed (landing root on another volume?); copying the full payload instead"
            );
            std::fs::copy(src, &dest).with_context(|| {
                format!(
                    "copy package payload {} -> {}",
                    src.display(),
                    dest.display()
                )
            })?;
        }
    }
    Ok(())
}

/// Terminal-cleanup sink for the collab sender map (Д4). A reconstructed serve
/// dir (`<sync_dir>/collab_serve/<package_id>`) is temporary and must be removed
/// once the serve reaches a terminal state; a retained publication
/// (`<sync_dir>/collab_pub/<package_id>`, an `origin='mine'` `local_dir`) MUST
/// survive so it can be re-served again — this sink never deletes it.
///
/// The discriminator is the parent directory name: only a dir whose parent is
/// exactly `collab_serve` is cleaned. Idempotent and cheap (called on the
/// synchronous engine worker at every terminal).
pub struct CollabCleanupSink;

impl PackageCleanupSink for CollabCleanupSink {
    fn on_terminal(&self, dir: &Path) {
        let under_serve = dir
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|n| n == std::ffi::OsStr::new("collab_serve"));
        if !under_serve {
            tracing::debug!(
                path = %dir.display(),
                "collab cleanup: retained dir left in place (not a collab_serve dir)"
            );
            return;
        }
        match std::fs::remove_dir_all(dir) {
            Ok(()) => tracing::info!(path = %dir.display(), "collab serve dir cleaned"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(path = %dir.display(), error = %e, "collab serve dir cleanup failed")
            }
        }
    }
}

/// Authorize + reconstruct the servable dir for a request, returning `Some(dir)`
/// when the serve may proceed and `None` (after a `warn!`) on any silent refusal.
/// The pure decision core of [`handle_project_request`] — no transport, no
/// `.await` — so it is exercised hermetically.
///
/// Refusals (all `Ok(None)`, warn-logged, cross-account so nothing goes back on
/// the wire): unknown package row; a package that is neither mine nor fully
/// received (`local_status != 'complete'`); a request naming a project other than
/// the package's own; a requester [`may_serve_package`](crate::collab::authz::may_serve_package)
/// refuses (a pending package ⇒ coordinator only, a published one ⇒ `send_receive`
/// member or coordinator).
fn authorize_and_reconstruct_serve(
    conn: &rusqlite::Connection,
    sync_dir: &Path,
    from: &NodeId,
    project_id: &str,
    package_id: &str,
) -> Result<Option<PathBuf>> {
    let Some(pkg) = crate::db::collab_exchange::get_package(conn, package_id)? else {
        tracing::warn!(
            project_id,
            package_id,
            from = %node_id_hex(from),
            "project serve refused: unknown package"
        );
        return Ok(None);
    };

    // Holdable = a package I published, or one I fully received (so I retained the
    // manifest bytes + landed the payloads to re-serve them).
    if pkg.origin != "mine" && pkg.local_status != "complete" {
        tracing::warn!(
            project_id,
            package_id,
            origin = %pkg.origin,
            local_status = %pkg.local_status,
            "project serve refused: package not locally complete"
        );
        return Ok(None);
    }

    // The request must name the package's OWN project — a coordinator of one
    // project can't pull another project's package by claiming its own id.
    if pkg.project_id != project_id {
        tracing::warn!(
            request_project = project_id,
            package_project = %pkg.project_id,
            package_id,
            "project serve refused: project mismatch"
        );
        return Ok(None);
    }

    // Authorization is against the package's authoritative project id. A still
    // `pending` package (hub has not published it) may be served ONLY to the
    // coordinator (they decide it); a published one to any `send_receive` member
    // or the coordinator.
    let pending = pkg.state == "pending";
    if !crate::collab::authz::may_serve_package(conn, &pkg.project_id, pending, from) {
        tracing::warn!(
            project_id = %pkg.project_id,
            package_id,
            pending,
            from = %node_id_hex(from),
            "project serve refused: requester not authorized to be served"
        );
        return Ok(None);
    }

    let dir = reconstruct_serve_dir(conn, sync_dir, package_id)?;
    Ok(Some(dir))
}

/// Holder side of Д1: authorize + reconstruct + enqueue an explicit-target serve
/// of `package_id` back to `from` through the collab sender map. `project_id` /
/// `package_id` come off the wire ([`TransportEvent::ProjectRequestReceived`](crate::sharing::types::TransportEvent),
/// where `package_id` is the HUB uuid). An authorization/eligibility failure is a
/// silent (warn-logged) drop — cross-account, so no error is returned to the
/// requester.
pub async fn handle_project_request(
    ctx: &ServiceContext,
    sender: &SyncSenderRuntime,
    from: NodeId,
    project_id: String,
    package_id: String,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<()> {
    // The reconstructed serve dir is collab DATA, so it follows the working dir.
    let working_dir = crate::api::sync::sync_dirs(ctx)?.working_dir;

    // Decide + reconstruct with the DB borrow scoped OUT before any `.await`.
    let dir = {
        let db = db(ctx)?;
        let conn = db.conn();
        match authorize_and_reconstruct_serve(&conn, &working_dir, &from, &project_id, &package_id)?
        {
            Some(dir) => dir,
            None => return Ok(()), // silently refused (already warned)
        }
    };

    // Serve: enqueue an explicit-target send of the reconstructed dir to `from`
    // through the dedicated collab sender map. The manifest carries the project
    // stamp, so the engine advertises via `announce_project` and skips the
    // Offer/Want dedup negotiation automatically (T1 behavior); terminal cleanup
    // routes through the `CollabCleanupSink`.
    let (engine, _origin) = ensure_collab_sender_engine(ctx, sender, from, emitter).await?;
    // Mirror-hierarchy T2: collab packages are out of the v1 mirror scope — a
    // project package lands through the collab ingest path, not the personal
    // `<sender>/<batch>/` tree, so the stamp stays `Batch` (the column default,
    // i.e. unchanged behavior).
    engine
        .enqueue_package(&dir, None, Vec::new(), PackageLayout::Batch)
        .await
        .with_context(|| format!("enqueue collab serve dir {}", dir.display()))?;
    tracing::info!(
        project_id = %project_id,
        package_id = %package_id,
        to = %node_id_hex(&from),
        "collab project serve enqueued"
    );
    Ok(())
}

/// Ensure the collab sender engine for `dest` is running and return its handle +
/// this device's origin id. The collab counterpart of
/// [`crate::api::sync::ensure_sender_engine`]: same transport-build shape (shared
/// device identity, resolved relay map, `dest` dial hint) but a DEDICATED
/// `<sync_dir>/blobs_collab` blob store — a second `FsStore` over the
/// personal-sync `blobs_out` would risk the redb lock and the sender's startup
/// tag-sweep (audit m7). Engines spawn via
/// [`SyncEngine::spawn_with_sink_and_emitter`] with the [`CollabCleanupSink`] so a
/// reconstructed serve dir is cleaned on terminal while a retained publication
/// survives confirm (Д4). Idempotent per destination (the runtime mutex is held
/// across the whole build so two concurrent requests can't spawn two engines).
pub async fn ensure_collab_sender_engine(
    ctx: &ServiceContext,
    sender: &SyncSenderRuntime,
    dest: NodeId,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<(Arc<SyncEngineHandle>, String), ApiError> {
    let mut guard = sender.lock_inner().await;
    if let Some(started) = guard.get(&dest) {
        return Ok((Arc::clone(&started.engine), started.origin_device.clone()));
    }

    let peer = dest;
    // Relay URLs for the dial hint (a bare account/membership-resolved dest is
    // undialable without one); the shared node resolves the relay MODE once.
    let (_relay_mode, relay_urls) = crate::api::sync::resolve_relay_mode(ctx).await?;
    let db_path = crate::api::sync::sync_dirs(ctx)?.db_path;

    // The ONE shared iroh node (C1 fix): the collab sender is its `Collab` role
    // handle, sharing the single endpoint + `<working>/blobs` store with the
    // receiver and the personal sender. Role-prefixed blob tags (Д3) keep the
    // three roles from clobbering each other's tags on the shared store — this
    // replaces the old dedicated `blobs_collab` `FsStore` (audit m7), which only
    // existed because each role used to bind its OWN endpoint + store.
    let node = crate::api::sync::ensure_iroh_node(ctx).await?;
    let transport: Arc<dyn SharingTransport> = node.handle(Role::Collab);
    let origin_device = node_id_hex(&node.node_id());

    // The destination is an account/membership-resolved bare node id. Attach our
    // own resolved relay URL(s) as its dial hint before the first announce (same
    // reasoning as the personal-sync sender).
    //
    // T8 decision (peer_addr_with_relays vs. the T7 peer_dial_addr, and the retry
    // refresher): the seed-target here is `dest` == the REQUESTER (`from`) of an
    // inbound `ProjectRequestReceived` — a cross-account collaborator. Its own
    // hub-reported `endpoint_addr` is genuinely NOT in scope at this call site: it
    // arrives as a bare node id off the wire, and the collab membership snapshot
    // carries pubkeys/roles, not per-member endpoint addresses (unlike the account
    // device list the personal sender reads). So there is no reported/holder relay
    // to prefer — `peer_addr_with_relays(peer, our_relays)` (relay-only, our set)
    // is the strongest hint available, and switching to `peer_dial_addr` would add
    // nothing without a reported address to pass it. For the same reason no
    // per-peer retry `AddrRefresher` is wired on the collab sender: a refresher
    // could only re-run this same our-relay-set resolution (the requester's real
    // address stays out of scope), and the node-level hourly relay rebuild (H2)
    // already re-establishes our relay reachability. If a per-member endpoint-
    // address endpoint is added later, revisit both here.
    let peer_addr = pairing::peer_addr_with_relays(peer, &relay_urls)
        .map_err(|e| ApiError::Internal(format!("construct peer address: {e:#}")))?;
    node.add_peer(peer_addr);

    let store = Arc::new(
        CatalogSyncStore::open(&db_path)
            .map_err(|e| ApiError::Internal(format!("open catalog sync store: {e:#}")))?,
    );
    let engine = Arc::new(SyncEngine::spawn_with_sink_and_emitter(
        store as Arc<dyn SyncStore>,
        transport,
        peer,
        Arc::new(CollabCleanupSink),
        emitter,
    ));

    tracing::info!(peer = %node_id_hex(&peer), origin = %origin_device, "collab sender engine started");
    guard.insert(
        dest,
        StartedSender {
            engine: Arc::clone(&engine),
            origin_device: origin_device.clone(),
            peer,
        },
    );
    Ok((engine, origin_device))
}

// ── Announcements poll + download orchestration (slice 4, task 8) ────────────
//
// The receive/coordinate side of the exchange: `refresh_project_packages` polls
// the hub's announcement list into `project_packages` (upsert + state diffs the
// frontend turns into `notify()`), and `download_project_package` runs the Д6
// explicit sequential-holder pull. Ungated, like the rest of this module.

/// A holder counts as ONLINE when the hub last saw it within this many seconds.
const HOLDER_ONLINE_WINDOW_SECS: i64 = 300;

/// Download loop cadence: re-read the row this often, up to the timeout, waiting
/// for a requested serve to ingest (the Task-5 receiver arm flips `local_status`).
const DOWNLOAD_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
const DOWNLOAD_POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Bound on the short control-connect reachability probe run against a holder
/// (Task 9) BEFORE committing to the [`DOWNLOAD_POLL_TIMEOUT`] blob poll — a dead
/// or refusing holder is skipped in seconds instead of stalling 90s.
const HOLDER_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// One state change a poll observed, which the frontend (Task 11) turns into a
/// `notify()` call. [`refresh_project_packages`] NEVER notifies itself.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PackageStateChange {
    pub project_id: String,
    pub package_id: String,
    /// `newPackage` | `approved` | `rejected` | `downloadFailed` | `awaitingApproval`.
    pub kind: String,
    pub detail: Option<String>,
}

/// Live progress of a SWARM download (D3 §3.1.3), emitted on
/// `project-download-progress`. Two events per attempt: `stage = "fetching"`
/// when the fan-out starts and `stage = "done"` when it ends (either way — the
/// outcome travels on `local_status`, as it always has). The UI clears its
/// "downloading from N sources" line on `done`.
///
/// `sources` is the PROVIDER COUNT this attempt was handed — the holder set
/// minus self — not a count derived from provider telemetry. The blob layer's
/// per-provider stream is lossy by construction (see [`ProviderTelemetrySink`]),
/// so counting distinct providers from it reads low and jittery; the list we
/// passed in is the honest figure.
///
/// The sequential fallback emits nothing here: it pulls from exactly one holder
/// at a time and already reports through the receiver's `sync-progress`.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDownloadProgress {
    pub project_id: String,
    pub package_id: String,
    /// How many holders the fan-out was handed.
    pub sources: usize,
    /// `fetching` | `done`.
    pub stage: String,
}

/// Map an [`AccountClientError`](crate::account::AccountClientError) onto the api
/// boundary (mirrors `api::collab::client_err`).
fn client_err(e: crate::account::AccountClientError) -> ApiError {
    use crate::account::AccountClientError as E;
    match e {
        E::RateLimited => {
            ApiError::Invalid("Too many requests — wait a minute and try again.".into())
        }
        E::Unauthorized => {
            ApiError::SignedOut("Signed out or device revoked — sign in again.".into())
        }
        E::SecondPrimary(m) | E::DeviceConflict(m) => ApiError::Conflict(m),
        E::PeerValidation(m) | E::BadRequest(m) => ApiError::Invalid(m),
        E::DuplicateName => ApiError::Invalid("name already in use".into()),
        E::Forbidden => {
            ApiError::Forbidden("The account's role may not perform this action.".into())
        }
        // The per-frame calls here (the version poll, the manifest sync)
        // produce this variant; the deprecated package-api ones never do.
        E::CollabApiOutdated => {
            crate::account::client::warn_collab_api_outdated_once();
            ApiError::Conflict(crate::account::client::COLLAB_API_OUTDATED_MSG.into())
        }
        E::Network(m) => ApiError::Internal(format!("Hub request failed: {m}")),
    }
}

/// Is this holder online — did the hub see it within [`HOLDER_ONLINE_WINDOW_SECS`]
/// of `now`? An absent or unparseable `last_seen_at` ⇒ offline.
fn holder_online(h: &HolderWire, now: chrono::DateTime<chrono::Utc>) -> bool {
    h.last_seen_at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| {
            (now - t.with_timezone(&chrono::Utc)).num_seconds().abs() <= HOLDER_ONLINE_WINDOW_SECS
        })
        .unwrap_or(false)
}

/// Diff each announcement against the existing row, then upsert it. Returns the
/// `PackageStateChange`s the poll observed. Pure (no network, no notify) so it is
/// exercised hermetically and reused by [`download_project_package`]'s fresh poll.
///
/// Diff rules (brief): an unknown row that is `published` and not mine ⇒
/// `newPackage`; an unknown row that is `pending` and not mine ⇒ `awaitingApproval`
/// (a coordinator's view of a contribution awaiting a decision — hub visibility
/// already restricts foreign pending rows to coordinators); a known OWN row moving
/// `pending → published` ⇒ `approved`; any known row moving to `rejected` ⇒
/// `rejected` (carrying the hub reason). The
/// upsert's forward-only origin + preserved local progress (T3) mean a poll can't
/// downgrade a received/owned row or clobber its manifest / `local_status`.
///
/// T3 hazard: `superseded` is a hub-mirrored column overwritten on EVERY upsert,
/// so this re-marks the complete union of the hub-listed `supersedes` arrays at
/// the end of the cycle — otherwise an own-published supersede flag would clear.
fn apply_announcements(
    conn: &rusqlite::Connection,
    project_id: &str,
    anns: &[AnnouncementWire],
) -> Result<Vec<PackageStateChange>, ApiError> {
    let now = chrono::Utc::now();
    let mut changes = Vec::new();
    let mut supersedes_union: Vec<String> = Vec::new();

    for ann in anns {
        let existing = get_package(conn, &ann.package_id)?;

        // Diff BEFORE the upsert overwrites the hub-mirrored state.
        match &existing {
            None => {
                if ann.state == "published" && !ann.own {
                    changes.push(PackageStateChange {
                        project_id: project_id.to_string(),
                        package_id: ann.package_id.clone(),
                        kind: "newPackage".to_string(),
                        detail: None,
                    });
                } else if ann.state == "pending" && !ann.own {
                    // A foreign pending row is a contribution awaiting a decision.
                    // Hub visibility already restricts foreign pending announcements
                    // to that project's coordinators, so seeing one here means we
                    // are a coordinator — no app-side role check is needed.
                    changes.push(PackageStateChange {
                        project_id: project_id.to_string(),
                        package_id: ann.package_id.clone(),
                        kind: "awaitingApproval".to_string(),
                        detail: None,
                    });
                }
            }
            Some(row) => {
                if ann.state == "rejected" && row.state != "rejected" {
                    changes.push(PackageStateChange {
                        project_id: project_id.to_string(),
                        package_id: ann.package_id.clone(),
                        kind: "rejected".to_string(),
                        detail: ann.reject_reason.clone(),
                    });
                } else if ann.own && row.state == "pending" && ann.state == "published" {
                    changes.push(PackageStateChange {
                        project_id: project_id.to_string(),
                        package_id: ann.package_id.clone(),
                        kind: "approved".to_string(),
                        detail: None,
                    });
                }
            }
        }

        let manifest_xxh3 = ann
            .aggregate_stats
            .get("manifestXxh3")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let online_count = ann.holders.iter().filter(|h| holder_online(h, now)).count() as i64;

        upsert_package(
            conn,
            &PackageRow {
                package_id: ann.package_id.clone(),
                project_id: project_id.to_string(),
                announcement_id: ann.id.clone(),
                publisher_display: ann.publisher_display_name.clone(),
                own: ann.own,
                root_hash: ann.root_hash.clone(),
                byte_size: ann.byte_size,
                frame_count: ann.frame_count as i64,
                manifest_xxh3,
                aggregate_stats: ann.aggregate_stats.to_string(),
                supersedes: serde_json::to_string(&ann.supersedes).unwrap_or_else(|_| "[]".into()),
                state: ann.state.clone(),
                reject_reason: ann.reject_reason.clone(),
                // Re-derived comprehensively below — never trust this per-row.
                superseded: false,
                origin: if ann.own { "mine" } else { "remote" }.to_string(),
                // Local-only columns: the upsert preserves the existing values, so
                // a poll never clobbers a retained dir / manifest / fetch progress.
                local_dir: None,
                manifest_ndjson: None,
                local_status: "none".to_string(),
                holder_count: ann.holders.len() as i64,
                online_count,
                created_at: ann.created_at.clone(),
                decided_at: ann.decided_at.clone(),
                fetched_at: String::new(),
            },
        )?;
        supersedes_union.extend(ann.supersedes.iter().cloned());
    }

    // Comprehensive re-mark (T3 hazard) — mark_superseded no-ops on an empty slice.
    mark_superseded(conn, &supersedes_union)?;
    Ok(changes)
}

/// Fetch one project's announcements from the hub and apply them. Returns the raw
/// wire announcements (holders included — the download loop needs them) plus the
/// state diffs. Signed out ⇒ `Ok((vec![], vec![]))` so the poll cadence degrades
/// quietly instead of erroring.
async fn poll_project_announcements(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<(Vec<AnnouncementWire>, Vec<PackageStateChange>), ApiError> {
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Ok((Vec::new(), Vec::new()));
    };
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    #[allow(deprecated)] // collab v3: `list_announcements` removed in wave 2 Task 11
    let anns = client
        .list_announcements(&token, project_id)
        .await
        .map_err(client_err)?;
    let changes = {
        let db = db(ctx)?;
        let conn = db.conn();
        apply_announcements(&conn, project_id, &anns)?
    };
    Ok((anns, changes))
}

/// Poll one project's announcements into `project_packages`. Returns the diffs the
/// frontend turns into `notify()` calls. NEVER notifies itself.
///
/// A poll that actually CHANGED something also kicks the auto-replication worker
/// (spec §3.3's "immediately after a hub poll that changed any project's package
/// set", F4) — without it a fresh approval waits out the 20-minute cadence.
pub async fn refresh_project_packages(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<Vec<PackageStateChange>, ApiError> {
    let (_anns, changes) = poll_project_announcements(ctx, project_id).await?;
    kick_auto_sync_if_changed(&changes);
    Ok(changes)
}

/// The wakeup the auto-replication worker waits on between passes (F4, spec
/// §3.3: a pass "immediately after a hub poll that changed any project's package
/// set"). A module static for the same reason [`SWARM_UNFIT`] and
/// [`PENDING_PACKAGE_CHANGES`] are: the producers ([`refresh_project_packages`],
/// reached from both hosts' poll commands) and the consumer (the worker spawned
/// by [`spawn_collab_auto_sync`]) have no shared owner, and a static keeps both
/// arming sites in `api::sync` and every command signature untouched.
static AUTO_SYNC_KICK: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();

fn auto_sync_kick() -> &'static tokio::sync::Notify {
    AUTO_SYNC_KICK.get_or_init(tokio::sync::Notify::new)
}

/// Kick the auto-replication worker iff `changes` is non-empty — i.e. only when
/// the refresh actually moved a project's package rows. Returns whether it
/// kicked (the unit-test oracle).
///
/// A no-op refresh must NOT kick: the poll cadence would then drive the worker
/// instead of the 20-minute interval it is designed around. Note the benign
/// self-feedback this bounds: a worker pass refreshes announcements itself, so a
/// pass that observes changes schedules exactly ONE follow-up pass, whose own
/// refresh sees nothing new and kicks nobody.
fn kick_auto_sync_if_changed(changes: &[PackageStateChange]) -> bool {
    if changes.is_empty() {
        return false;
    }
    // `notify_one` with no waiter STORES a permit, so a kick that lands while a
    // pass is running is not lost — the next wait consumes it immediately — and
    // several kicks in one window collapse into a single follow-up pass.
    auto_sync_kick().notify_one();
    tracing::debug!(
        count = changes.len(),
        "collab auto-sync kicked by a package-set change"
    );
    true
}

// ── Version poll + manifest delta (collab v3 wave 2, Task 8; R19, P9) ────────
//
// The hub keeps one `version` per project, bumped by every change a device
// must see (a manifest row, membership, caps, thresholds, dictionary). The
// auto-sync worker asks `GET /me/project-versions` every
// [`COLLAB_VERSION_POLL_INTERVAL`]; a project whose version differs from the
// cached `hub_version` gets ONE project refresh (joins, losses, caps,
// thresholds, dictionary) and a paged manifest delta from its cursor. A quiet
// poll is one request.

/// How often the auto-sync worker asks the hub whether any project moved.
pub const COLLAB_VERSION_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// Grace before the FIRST version poll of a session (the pass waits 90 s).
const COLLAB_VERSION_POLL_STARTUP_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

/// Rows per manifest page — the hub's cap.
const MANIFEST_PAGE_LIMIT: u32 = 1000;

/// The per-kind outcome event of a manifest sync ([`CollabFramesChange`]).
pub const COLLAB_FRAMES_CHANGED_EVENT: &str = "collab-frames-changed";

/// What a manifest delta changed, from this device's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum FramesChangeKind {
    /// Another member's frame became visible as published.
    NewFrames,
    /// Another member's frame awaits moderation (visible to moderators only).
    PendingFrames,
    /// My pending frame was published.
    Approved,
    /// My frame was rejected.
    Rejected,
    /// A frame was excluded from the project (`accepted` true → false).
    Excluded,
    /// Another member published a new content version of a frame.
    NewVersions,
}

impl FramesChangeKind {
    /// The wire spelling (the serde name), for logs.
    fn as_str(self) -> &'static str {
        match self {
            FramesChangeKind::NewFrames => "newFrames",
            FramesChangeKind::PendingFrames => "pendingFrames",
            FramesChangeKind::Approved => "approved",
            FramesChangeKind::Rejected => "rejected",
            FramesChangeKind::Excluded => "excluded",
            FramesChangeKind::NewVersions => "newVersions",
        }
    }
}

/// One `collab-frames-changed` event: how many rows of one kind a manifest
/// sync applied for one project. Emitted once per non-zero kind.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabFramesChange {
    pub project_id: String,
    pub kind: FramesChangeKind,
    pub count: usize,
}

/// Classify one manifest row against the local row it replaces (`None` =
/// never seen). Pure. A row can carry more than one kind (an exclusion and a
/// new version in one delta).
pub(crate) fn classify_frame_change(
    prev: Option<&crate::db::collab_frames::LocalFrameRow>,
    v: &crate::collab::hub_client::FrameViewWire,
) -> Vec<FramesChangeKind> {
    use FramesChangeKind as K;
    let mut out = Vec::new();
    let prev_state = prev.map(|p| p.state.as_str());
    if v.own {
        // Only a row this device already tracked: a first sync of an old
        // account must not replay every historical decision as news.
        if prev_state == Some("pending") && v.state == "published" {
            out.push(K::Approved);
        }
        if prev.is_some() && prev_state != Some("rejected") && v.state == "rejected" {
            out.push(K::Rejected);
        }
    } else {
        if v.state == "published" && prev_state != Some("published") {
            out.push(K::NewFrames);
        } else if prev.is_some_and(|p| v.content_version > p.content_version) {
            out.push(K::NewVersions);
        }
        // Pending rows of another member reach only a moderator's manifest.
        if v.state == "pending" && prev_state != Some("pending") {
            out.push(K::PendingFrames);
        }
    }
    if prev.is_some_and(|p| p.accepted) && !v.accepted {
        out.push(K::Excluded);
    }
    out
}

/// Pull one project's manifest delta into `project_frames_local` (P9).
///
/// 1. The cursor is `manifest_cursor`, or 0 with a prune when the caps the
///    last sync saw (`synced_caps_json`) differ from the current ones
///    (`gov_caps_json`, written by the project refresh) — the hub README's
///    client caps rule.
/// 2. Pages of 1000 are followed through `next` until `hasMore` is false;
///    each row is classified against the local row, then upserted (an own
///    row takes the hub's columns and keeps its local ones).
/// 3. On a caps change, every replica row the fresh fetch did not return is
///    deleted; own rows never are.
/// 4. The sync state records the highest `manifestVersion` applied and the
///    caps this sync ran under. `hub_version` becomes `vouched_version` —
///    the `/me/project-versions` value the version poll acted on — and stays
///    as it is when `None` (ruling R12): the manifest's own `projectVersion`
///    also covers changes only a project refresh picks up (membership, caps,
///    thresholds, dictionary), so it never vouches for anything.
/// 5. `collab-frames-changed` is emitted once per non-zero kind — also for
///    the rows a failed sync applied before it stopped (they are applied
///    locally, and the retry will not see them as changes again).
///
/// Syncs of one project are serialized (a per-project async lock), so a
/// poll-side and a pass-side sync never apply — and emit — the same delta
/// twice. A failure is logged here and leaves the sync state untouched, so
/// the next sync starts from the same cursor; the upserts it already made
/// are idempotent.
pub async fn sync_manifest(
    ctx: &ServiceContext,
    project_id: &str,
    emitter: Option<&dyn ProgressEmitter>,
    vouched_version: Option<i64>,
) -> Result<Vec<CollabFramesChange>, ApiError> {
    let result = sync_manifest_serialized(ctx, project_id, emitter, vouched_version).await;
    if let Err(e) = &result {
        tracing::warn!(
            project_id,
            error = %e,
            "manifest sync failed; the next sync resumes from the stored cursor"
        );
    }
    result
}

/// The lock for one project's manifest syncs, keyed by hub + project.
fn manifest_sync_lock(key: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(locks.entry(key.to_string()).or_default())
}

/// `<catalog path>|<hub url>|` — the scope of every per-project static in
/// this section, so two app contexts (the e2e harness runs three in one
/// process, against one hub) or two hubs never share state.
fn poll_scope(ctx: &ServiceContext, hub_url: &str) -> Result<String, ApiError> {
    Ok(format!("{}|{hub_url}|", db(ctx)?.path().display()))
}

async fn sync_manifest_serialized(
    ctx: &ServiceContext,
    project_id: &str,
    emitter: Option<&dyn ProgressEmitter>,
    vouched_version: Option<i64>,
) -> Result<Vec<CollabFramesChange>, ApiError> {
    use crate::db::collab_frames as frames_db;

    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Err(ApiError::SignedOut(
            "Sign in to use collaboration projects.".into(),
        ));
    };
    let lock = manifest_sync_lock(&format!("{}{project_id}", poll_scope(ctx, &hub_url)?));
    let _serial = lock.lock().await;

    let project = {
        let database = db(ctx)?;
        let conn = database.conn();
        crate::db::collab::get_project(&conn, project_id)?
    };
    let Some(project) = project else {
        return Err(ApiError::NotFound(format!(
            "project {project_id} is not cached — refresh first"
        )));
    };
    let client = CollabClient::new(&hub_url).map_err(client_err)?;

    let caps_changed = project.gov_caps_json != project.synced_caps_json;
    let start = if caps_changed {
        0
    } else {
        project.manifest_cursor
    };
    let mut counts: std::collections::BTreeMap<FramesChangeKind, usize> =
        std::collections::BTreeMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut max_mv = project.manifest_cursor;
    let mut applied = 0usize;

    let fetched: Result<(), ApiError> = async {
        let mut since = start;
        let mut after: Option<String> = None;
        loop {
            let page = client
                .manifest_page(
                    &token,
                    project_id,
                    since,
                    after.as_deref(),
                    MANIFEST_PAGE_LIMIT,
                )
                .await
                .map_err(client_err)?;
            {
                let database = db(ctx)?;
                let conn = database.conn();
                let tx = conn.unchecked_transaction()?;
                for v in &page.rows {
                    let prev = frames_db::get(&tx, project_id, &v.frame_uuid)?;
                    for kind in classify_frame_change(prev.as_ref(), v) {
                        *counts.entry(kind).or_default() += 1;
                    }
                    frames_db::upsert_from_manifest(&tx, project_id, v)?;
                    seen.insert(v.frame_uuid.clone());
                    max_mv = max_mv.max(v.manifest_version);
                }
                tx.commit()?;
            }
            applied += page.rows.len();
            if !page.has_more {
                return Ok(());
            }
            match page.next {
                Some(next) => {
                    since = next.since;
                    after = Some(next.after);
                }
                None => {
                    return Err(ApiError::Internal(
                        "hub manifest page has more rows but no cursor".into(),
                    ))
                }
            }
        }
    }
    .await;

    let changes: Vec<CollabFramesChange> = counts
        .into_iter()
        .map(|(kind, count)| CollabFramesChange {
            project_id: project_id.to_string(),
            kind,
            count,
        })
        .collect();
    for change in &changes {
        tracing::info!(
            project_id,
            count = change.count,
            kind = change.kind.as_str(),
            "manifest delta applied"
        );
        if let Some(em) = emitter {
            crate::events::emit_event(em, COLLAB_FRAMES_CHANGED_EVENT, change);
        }
    }
    fetched?;

    let pruned = {
        let database = db(ctx)?;
        let conn = database.conn();
        let pruned = if caps_changed {
            frames_db::delete_not_in(&conn, project_id, &seen)?
        } else {
            0
        };
        crate::db::collab::set_sync_state(
            &conn,
            project_id,
            vouched_version,
            max_mv,
            &project.gov_caps_json,
        )?;
        pruned
    };
    tracing::debug!(
        project_id,
        count = applied,
        pruned,
        caps_changed,
        hub_version = ?vouched_version,
        manifest_cursor = max_mv,
        "manifest synced"
    );
    Ok(changes)
}

/// One version poll (R19).
///
/// - Signed out → `Ok(vec![])`, as the pass does.
/// - `GET /me/project-versions`. A project is a TARGET when it is listed but
///   not cached at that version (moved or joined), or cached and no longer
///   listed (lost). A target in back-off (ruling R13) is skipped. With no
///   target left, that is the whole poll: one request.
/// - Otherwise ONE project refresh limited to the moved projects (a loss is
///   seen from the list), then [`sync_manifest`] for each moved project the
///   refresh actually refreshed, vouching for the listed version.
/// - A project whose refresh or sync fails backs off to the pass cadence
///   ([`COLLAB_AUTO_SYNC_INTERVAL`]) instead of costing hub requests every
///   tick; the back-off is logged when it starts and when it ends. The
///   failure itself was logged where it happened.
/// - [`crate::api::collab::on_thresholds_or_dictionary_moved`] fires for
///   every project whose thresholds or dictionary moved in the refresh,
///   after the syncs.
/// - Returns the moved project ids whose manifest synced (joins included,
///   losses not); the caller kicks the pass when the list is non-empty.
/// - A `collab_api_outdated` refusal is the P17 `Conflict`, logged once.
#[cfg(all(feature = "render", feature = "solver"))]
pub async fn poll_versions_once(
    ctx: &ServiceContext,
    emitter: Option<&dyn ProgressEmitter>,
) -> Result<Vec<String>, ApiError> {
    use std::collections::HashMap;

    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        tracing::debug!("collab version poll: signed out; skipped");
        return Ok(Vec::new());
    };
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    let versions = client.project_versions(&token).await.map_err(client_err)?;

    let cached: HashMap<String, i64> = {
        let database = db(ctx)?;
        let conn = database.conn();
        crate::db::collab::list_projects(&conn)?
            .into_iter()
            .map(|r| (r.project_id, r.hub_version))
            .collect()
    };
    let scope = poll_scope(ctx, &hub_url)?;
    let key = |project_id: &str| format!("{scope}{project_id}");
    let listed: HashSet<&str> = versions.iter().map(|v| v.project_id.as_str()).collect();
    let moved_all: Vec<(String, i64)> = versions
        .iter()
        .filter(|v| cached.get(&v.project_id) != Some(&v.version))
        .map(|v| (v.project_id.clone(), v.version))
        .collect();
    let lost_all: Vec<String> = cached
        .keys()
        .filter(|id| !listed.contains(id.as_str()))
        .cloned()
        .collect();
    let moved: Vec<(String, i64)> = moved_all
        .into_iter()
        .filter(|(id, _)| !backoff_active(&key(id)))
        .collect();
    let lost: Vec<String> = lost_all
        .into_iter()
        .filter(|id| !backoff_active(&key(id)))
        .collect();
    if moved.is_empty() && lost.is_empty() {
        tracing::debug!("collab version poll: nothing to do");
        return Ok(Vec::new());
    }
    tracing::debug!(
        count = moved.len(),
        lost = lost.len(),
        "collab version poll: versions moved; refreshing projects"
    );

    let only: HashSet<String> = moved.iter().map(|(id, _)| id.clone()).collect();
    let report = match crate::api::collab::refresh_projects_reporting(ctx, Some(&only)).await {
        Ok(report) => report,
        Err(e) => {
            for id in only.iter().chain(lost.iter()) {
                back_off(&key(id), id, "project refresh failed");
            }
            return Err(e);
        }
    };
    for id in &lost {
        clear_backoff(&key(id), id);
    }

    let mut synced = Vec::new();
    for (project_id, version) in &moved {
        if !report.refreshed.contains(project_id) {
            // Syncing now would vouch for a version whose caps / thresholds /
            // dictionary the cache never saw.
            back_off(&key(project_id), project_id, "project refresh failed");
            continue;
        }
        match sync_manifest(ctx, project_id, emitter, Some(*version)).await {
            Ok(_) => {
                clear_backoff(&key(project_id), project_id);
                synced.push(project_id.clone());
            }
            Err(_) => back_off(&key(project_id), project_id, "manifest sync failed"),
        }
    }
    for project_id in &report.gate_moved {
        crate::api::collab::on_thresholds_or_dictionary_moved(ctx, project_id);
    }
    Ok(synced)
}

/// Per-project back-off (ruling R13): `catalog|hub|project` → the instant before
/// which the version poll leaves the project alone. An entry that has
/// expired stays until a success clears it, so a repeated failure is not
/// logged as a new one.
#[cfg(all(feature = "render", feature = "solver"))]
static POLL_BACKOFF: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
> = std::sync::OnceLock::new();

#[cfg(all(feature = "render", feature = "solver"))]
fn poll_backoff(
) -> std::sync::MutexGuard<'static, std::collections::HashMap<String, std::time::Instant>> {
    POLL_BACKOFF
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Is the poll still leaving this project alone?
#[cfg(all(feature = "render", feature = "solver"))]
fn backoff_active(key: &str) -> bool {
    poll_backoff()
        .get(key)
        .is_some_and(|until| std::time::Instant::now() < *until)
}

/// Back a project off to the pass cadence; logged when the back-off starts.
#[cfg(all(feature = "render", feature = "solver"))]
fn back_off(key: &str, project_id: &str, outcome: &str) {
    let started = poll_backoff()
        .insert(
            key.to_string(),
            std::time::Instant::now() + COLLAB_AUTO_SYNC_INTERVAL,
        )
        .is_none();
    if started {
        tracing::info!(
            project_id,
            outcome,
            retry_secs = COLLAB_AUTO_SYNC_INTERVAL.as_secs(),
            "collab version poll: project backs off to the pass cadence"
        );
    } else {
        tracing::debug!(
            project_id,
            outcome,
            "collab version poll: project still failing"
        );
    }
}

/// End a project's back-off after a success; logged when there was one.
#[cfg(all(feature = "render", feature = "solver"))]
fn clear_backoff(key: &str, project_id: &str) {
    if poll_backoff().remove(key).is_some() {
        tracing::info!(project_id, "collab version poll: project recovered");
    }
}

/// Forget every back-off of one app context (tests: skip the 20-minute wait).
#[cfg(all(test, feature = "render", feature = "solver"))]
pub(crate) fn clear_poll_backoff_for(ctx: &ServiceContext) {
    let prefix = format!("{}|", db(ctx).unwrap().path().display());
    poll_backoff().retain(|k, _| !k.starts_with(&prefix));
}

/// Kick the pass iff the poll saw a version move. Returns whether it kicked
/// (the unit-test oracle). Same stored-permit semantics as
/// [`kick_auto_sync_if_changed`].
#[cfg(all(feature = "render", feature = "solver"))]
fn kick_if_versions_moved(moved: &[String]) -> bool {
    if moved.is_empty() {
        return false;
    }
    auto_sync_kick().notify_one();
    tracing::debug!(
        count = moved.len(),
        "collab auto-sync kicked by a project version move"
    );
    true
}

/// Whether the last whole version poll failed — so a hub outage logs one
/// `warn!` when it starts and one `info!` when it ends, not one per tick.
#[cfg(all(feature = "render", feature = "solver"))]
static POLL_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// One tick of the version poll, bound to the worker's context and emitter:
/// poll, then kick the pass when a version moved. Logged at `debug` (it runs
/// 5 760 times a day); a whole-poll failure that starts or ends is logged
/// once at `warn!` / `info!`.
async fn version_poll_tick(ctx: Arc<ServiceContext>, emitter: Option<Arc<dyn ProgressEmitter>>) {
    #[cfg(all(feature = "render", feature = "solver"))]
    {
        use std::sync::atomic::Ordering;
        match poll_versions_once(&ctx, emitter.as_deref()).await {
            Ok(moved) => {
                if POLL_DOWN.swap(false, Ordering::SeqCst) {
                    tracing::info!("collab version poll recovered");
                }
                kick_if_versions_moved(&moved);
            }
            Err(e) => {
                if !POLL_DOWN.swap(true, Ordering::SeqCst) {
                    tracing::warn!(error = %e, "collab version poll failed; retrying every tick");
                } else {
                    tracing::debug!(error = %e, "collab version poll still failing");
                }
            }
        }
    }
    #[cfg(not(all(feature = "render", feature = "solver")))]
    {
        // The project refresh lives in the render+solver-gated `api::collab`;
        // a headless build has no version poll.
        let _ = (ctx, emitter);
    }
}

/// The version poll's own loop (ruling R15): first tick after
/// `startup_delay`, then every `interval` (a slow tick delays the next
/// instead of bursting). It only ever kicks the pass; it never waits for
/// one, so a long pass does not pause it.
async fn version_poll_loop<P, PFut>(
    startup_delay: std::time::Duration,
    interval: std::time::Duration,
    run_poll: P,
) where
    P: Fn() -> PFut,
    PFut: std::future::Future<Output = ()>,
{
    tokio::time::sleep(startup_delay).await;
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        run_poll().await;
    }
}

/// All cached projects (the poll-cadence entry point). A per-project failure is
/// logged and skipped so one unreachable project never sinks the whole sweep.
pub async fn refresh_all_project_packages(
    ctx: &ServiceContext,
) -> Result<Vec<PackageStateChange>, ApiError> {
    let project_ids: HashSet<String> = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab::list_projects(&conn)?
            .into_iter()
            .map(|p| p.project_id)
            .collect()
    };
    let mut all = Vec::new();
    for pid in &project_ids {
        match refresh_project_packages(ctx, pid).await {
            Ok(mut c) => all.append(&mut c),
            Err(e) => {
                tracing::warn!(project_id = %pid, error = %format!("{e}"), "refresh project packages failed; continuing")
            }
        }
    }
    // Surface every change buffered off this path — a spawned pull task's
    // `downloadFailed` (F3) and the auto-replication worker's own refresh diffs
    // (D3 T5) — drained exactly once so the frontend raises each one only once,
    // and only for projects this device still has (F7).
    all.extend(drain_pending_package_changes(&project_ids));
    Ok(all)
}

// ── Cache-only list views (Task 11) ──────────────────────────────────────────
//
// Both read `project_packages` / `project_contributions` rows the poll (Task 8)
// already populated — no hub I/O — and project each row down to the fields the
// Stage-II UI (Task 12) renders. Instant, offline-safe reads.

/// One known package for a project, projected for the packages list. The two
/// swarm counts (`holder_count`/`online_count`) come from the Task-3 columns the
/// poll captures at announcement time (Task 8 writes them, this reads them).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPackageView {
    /// HUB package uuid (the `project_packages` row key).
    pub package_id: String,
    /// Hub-mirrored decision: `pending` | `published` | `rejected`.
    pub state: String,
    /// Local fetch progress: `none` | `downloading` | `complete` | `failed`.
    pub local_status: String,
    /// I published this package.
    pub own: bool,
    pub publisher: String,
    pub byte_size: i64,
    pub frame_count: i64,
    pub created_at: String,
    pub reject_reason: Option<String>,
    /// Another announcement supersedes this one.
    pub superseded: bool,
    /// Holders the hub listed at poll time.
    pub holder_count: i64,
    /// Of those holders, how many the hub last saw within the online window.
    pub online_count: i64,
}

impl From<PackageRow> for ProjectPackageView {
    fn from(r: PackageRow) -> Self {
        ProjectPackageView {
            package_id: r.package_id,
            state: r.state,
            local_status: r.local_status,
            own: r.own,
            publisher: r.publisher_display,
            byte_size: r.byte_size,
            frame_count: r.frame_count,
            created_at: r.created_at,
            reject_reason: r.reject_reason,
            superseded: r.superseded,
            holder_count: r.holder_count,
            online_count: r.online_count,
        }
    }
}

/// One received frame for a project, projected for the contributions list. These
/// rows never enter `files`/`frames` — they live only in `project_contributions`.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ContributionView {
    pub package_id: String,
    pub frame_uuid: String,
    pub publisher: String,
    pub rel_path: String,
    pub byte_size: i64,
    /// A newer contribution for the same frame uuid supersedes this one.
    pub superseded: bool,
    pub created_at: String,
}

impl From<ContributionRow> for ContributionView {
    fn from(r: ContributionRow) -> Self {
        ContributionView {
            package_id: r.package_id,
            frame_uuid: r.frame_uuid,
            publisher: r.publisher_display,
            rel_path: r.rel_path,
            byte_size: r.byte_size,
            superseded: r.superseded,
            created_at: r.created_at,
        }
    }
}

/// Every known package for a project (cache-only — no hub call). Newest
/// announcement first (the `list_packages` order). The poll (Task 8) keeps the
/// set and its swarm counts current.
pub fn list_project_packages(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<Vec<ProjectPackageView>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let rows = list_packages(&conn, project_id)?;
    Ok(rows.into_iter().map(ProjectPackageView::from).collect())
}

/// Every received contribution for a project (cache-only — no hub call), oldest
/// first (the `contributions_for_project` order).
pub fn list_contributions(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<Vec<ContributionView>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let rows = contributions_for_project(&conn, project_id)?;
    Ok(rows.into_iter().map(ContributionView::from).collect())
}

/// Report to the hub that this device now holds `package_id`'s blobs — the
/// task-8 post-ingest hook. Resolves the announcement id from the local row, then
/// `POST /announcements/{id}/have` with the device bearer. Signed out or an
/// unknown local row ⇒ a benign no-op (nothing to report against).
pub async fn report_have_after_ingest(
    ctx: &ServiceContext,
    package_id: &str,
) -> Result<(), ApiError> {
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Ok(());
    };
    let announcement_id = {
        let db = db(ctx)?;
        let conn = db.conn();
        let Some(row) = get_package(&conn, package_id)? else {
            tracing::warn!(package_id, "report_have: no local package row; skipping");
            return Ok(());
        };
        // Gate (F1): only advertise a package we can ACTUALLY fully serve. The
        // post-ingest hook fires even after a partial/failed ingest, so without
        // this the hub would list us as a holder that per-frame-fails every
        // requester. `ManifestNotCovered` is the anomaly (complete, yet the
        // content doesn't back it up) and warrants a `warn!`; the other two
        // non-held states are the ordinary course of a package still in
        // flight and only `debug!`.
        let held = package_fully_held(&conn, &row)
            .map_err(|e| ApiError::Internal(format!("package fully-held check: {e:#}")))?;
        match held {
            HeldState::Held => {}
            HeldState::ManifestNotCovered => {
                tracing::warn!(
                    package_id,
                    "report_have skipped: retained manifest not fully covered by local payloads"
                );
                return Ok(());
            }
            HeldState::NotComplete | HeldState::NoManifest => {
                tracing::debug!(
                    package_id,
                    local_status = %row.local_status,
                    "report_have skipped: package not fully held"
                );
                return Ok(());
            }
        }
        row.announcement_id
    };
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    #[allow(deprecated)] // collab v3: `report_have` removed in wave 2 Task 11
    client
        .report_have(&token, &announcement_id)
        .await
        .map_err(client_err)?;
    tracing::info!(package_id, announcement_id = %announcement_id, "reported have to hub");
    Ok(())
}

/// **Every downloader becomes a seed** (D3 §3.4): after a successful ingest, pin
/// the package's blobs under `project/<project_id>/<package_id>` so an incoming
/// swarm GET is served straight out of the blob store — no control-message round
/// trip, no reconstruct-on-demand, no `may_serve_package` handshake.
///
/// ONE hook for BOTH ingest completions: the receiver's push/fallback arm (via
/// `api::sync`'s `on_project_ingested` hook, which calls this before
/// [`report_have_after_ingest`]) and the swarm path
/// ([`download_project_package`], which calls it before its own `report_have`).
/// A third caller is [`seed_approved_announcement`], for the copy that was
/// ingested while still pending and only becomes seedable at the decision.
///
/// **PUBLISHED packages only** (F2). A seeded collection is served by the
/// provider machinery to anyone past the connect gate — no
/// [`may_serve_package`](crate::collab::authz::may_serve_package) check runs on a
/// raw blob GET, which is why the swarm path itself only ever fetches `published`
/// packages (spec §5). A coordinator's PENDING review copy is ingested through
/// this same hook, so seeding on `local_status` alone would hand every member of
/// the project a way around the pending ⇒ coordinator-only serve rule that spec
/// §6 says still governs who may pull. Approval is what lifts the gate.
///
/// **Order is load-bearing: seed, THEN report_have.** `report_have` is what puts
/// this device on the hub's holder list, i.e. what makes other members' swarm
/// fetches dial us; advertising before the blobs are servable would publish a
/// phantom holder every fetch has to fail over.
///
/// **Best-effort by design, and that is not a lie.** A seed failure is a `warn!`
/// and the caller still reports have — because a device with the package landed
/// can serve it via the pre-D3 path regardless (`handle_project_request`
/// reconstructs and enqueues on demand), which is exactly today's semantics. What
/// is lost on failure is the zero-round-trip swarm serve, not the holder claim.
///
/// Reads the ALREADY-BOUND node off the context and never binds one: seeding is a
/// side effect of an ingest that already happened, and a no-node context (loopback
/// tests, a host with sync not started) must not grow an endpoint for it.
pub async fn seed_ingested_package(ctx: &ServiceContext, package_id: &str) {
    let Some(node) = ctx.iroh_node.lock().await.clone() else {
        tracing::debug!(package_id, "project seed skipped: no iroh node bound");
        return;
    };
    // Seed dirs are collab DATA — they follow the working dir.
    let working_dir = match crate::api::sync::sync_dirs(ctx) {
        Ok(dirs) => dirs.working_dir,
        Err(e) => {
            tracing::warn!(package_id, error = %format!("{e}"), "project seed skipped: sync paths unavailable");
            return;
        }
    };

    // Same completeness gate `report_have_after_ingest` applies (F1): the hook
    // fires after a partial ingest too, and a package whose payloads are not all
    // local cannot be reconstructed — seeding it would pin a broken collection.
    let (project_id, dir) = {
        let db = match db(ctx) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(package_id, error = %format!("{e}"), "project seed skipped: catalog unavailable");
                return;
            }
        };
        let conn = db.conn();
        let row = match get_package(&conn, package_id) {
            Ok(Some(row)) => row,
            Ok(None) => {
                tracing::warn!(package_id, "project seed skipped: no local package row");
                return;
            }
            Err(e) => {
                tracing::warn!(package_id, error = %format!("{e}"), "project seed skipped: package read failed");
                return;
            }
        };
        if row.local_status != "complete" {
            tracing::debug!(
                package_id,
                local_status = %row.local_status,
                "project seed skipped: package not locally complete"
            );
            return;
        }
        // The state gate (F2): a pending review copy stays unseeded until it is
        // decided — see the doc above.
        if row.state != "published" {
            tracing::debug!(
                package_id,
                state = %row.state,
                "project seed skipped: package not published"
            );
            return;
        }
        match reconstruct_seed_dir(&conn, &working_dir, package_id) {
            Ok(dir) => (row.project_id, dir),
            Err(e) => {
                tracing::warn!(package_id, error = %format!("{e:#}"), "project seed skipped: seed dir unavailable");
                return;
            }
        }
    };

    match node
        .seed_project_collection(&project_id, package_id, &dir)
        .await
    {
        Ok(hash) => tracing::info!(
            project_id = %project_id,
            package_id,
            root_hash = %hash,
            "ingested package seeded"
        ),
        Err(e) => tracing::warn!(
            project_id = %project_id,
            package_id,
            error = %format!("{e:#}"),
            "seeding an ingested package failed; serving falls back to on-demand reconstruction"
        ),
    }
}

/// Seed the package an APPROVAL just published (F2) — the other half of the
/// state gate [`seed_ingested_package`] applies.
///
/// A coordinator's review copy lands while the announcement is still `pending`,
/// so its post-ingest seed is skipped; approval is the moment it becomes
/// servable to the project, and nothing else would seed it (the need diff only
/// pulls packages that are NOT locally complete, so no later pass revisits it).
/// Called from `api::collab::decide_announcement`'s approve branch AFTER the row
/// flips to `published`.
///
/// Best-effort with the ingest hook's exact contract: an unknown announcement, a
/// package that is not locally complete, and a failed import are each a log line,
/// never an error — a seed failure must not turn a successful moderation
/// decision into a reported failure, and the package stays servable through the
/// on-demand `handle_project_request` path either way.
pub async fn seed_approved_announcement(ctx: &ServiceContext, announcement_id: &str) {
    let package_id = {
        let db = match db(ctx) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(announcement_id, error = %format!("{e}"), "approved-package seed skipped: catalog unavailable");
                return;
            }
        };
        let conn = db.conn();
        match crate::db::collab_exchange::get_package_by_announcement(&conn, announcement_id) {
            Ok(Some(row)) => row.package_id,
            Ok(None) => {
                tracing::debug!(
                    announcement_id,
                    "approved-package seed skipped: no local package row"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(announcement_id, error = %format!("{e}"), "approved-package seed skipped: package read failed");
                return;
            }
        }
    };
    seed_ingested_package(ctx, &package_id).await;
}

/// Stop seeding ONE package and drop the materialized seed dir that backed it —
/// the teardown half of [`seed_ingested_package`], for the sites that delete a
/// package's local data (D3 §3.4: "deleting a project's local data deletes its
/// `project/<id>/…` tags in the same operation").
///
/// Best-effort and silent about absence: a package that was never seeded, a node
/// that is not bound, and a seed dir that does not exist are all normal. The seed
/// dir is removed because it is derived (hard links rebuilt by
/// [`reconstruct_seed_dir`] from the manifest + contributions); leaving it behind
/// an untagged collection would be pure garbage. An `origin='mine'` package's seed
/// dir IS its retained `local_dir`, which lives under `collab_pub` and is
/// therefore never matched here — its own publish path owns that dir's lifetime.
pub async fn unseed_package_local_data(ctx: &ServiceContext, project_id: &str, package_id: &str) {
    let Some(node) = ctx.iroh_node.lock().await.clone() else {
        tracing::debug!(project_id, package_id, "unseed skipped: no iroh node bound");
        return;
    };
    node.unseed_project_package(project_id, package_id).await;
    if let Ok(dirs) = crate::api::sync::sync_dirs(ctx) {
        remove_seed_dir(&dirs.working_dir, package_id);
    }
}

/// Stop seeding EVERY package of one project + drop their seed dirs — the
/// project-scoped twin of [`unseed_package_local_data`], for the site where this
/// device stops being a member of the project at all.
pub async fn unseed_project_local_data(ctx: &ServiceContext, project_id: &str) {
    let Some(node) = ctx.iroh_node.lock().await.clone() else {
        tracing::debug!(project_id, "unseed skipped: no iroh node bound");
        return;
    };
    node.unseed_project(project_id).await;
    let Ok(dirs) = crate::api::sync::sync_dirs(ctx) else {
        return;
    };
    let package_ids: Vec<String> = match db(ctx) {
        Ok(d) => match list_packages(&d.conn(), project_id) {
            Ok(rows) => rows.into_iter().map(|r| r.package_id).collect(),
            Err(e) => {
                tracing::warn!(project_id, error = %format!("{e}"), "unseed: listing the project's packages failed; seed dirs left in place");
                Vec::new()
            }
        },
        Err(e) => {
            tracing::warn!(project_id, error = %format!("{e}"), "unseed: catalog unavailable; seed dirs left in place");
            Vec::new()
        }
    };
    for package_id in package_ids {
        remove_seed_dir(&dirs.working_dir, &package_id);
    }
}

/// Best-effort removal of one `collab_seed/<package_id>` tree. Missing is normal
/// (never seeded, or already cleaned); anything else is logged, never swallowed.
fn remove_seed_dir(sync_dir: &Path, package_id: &str) {
    let dir = sync_dir.join(SEED_DIR).join(package_id);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => tracing::debug!(package_id, path = %dir.display(), "collab seed dir removed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(package_id, path = %dir.display(), error = %e, "collab seed dir cleanup failed")
        }
    }
}

// ── D3 §3.1: the swarm (multi-source) download path ─────────────────────────
//
// The preferred path in front of the sequential holder loop: hand the WHOLE
// holder set to one `fetch_collection_multi`, which splits the collection per
// child blob across every provider with byte-level failover. Everything below is
// try-then-fall-back by design — see `swarm_unfit` for why shape alone can never
// decide whether a swarm fetch is possible.

/// Where a swarm fetch stages the collection it exports, per package:
/// `<sync_dir>/collab_swarm/<package_id>`.
///
/// Deliberately NOT the receiver's `<sync_dir>/staging/<wire_id>` (spec §3.1.4
/// says "the same staging layout", which this is — one dir per package under the
/// sync dir — but not the same DIRECTORY). A received package's serve dir is
/// `collab_serve/<hub_package_id>`, so the wire id a holder announces for it IS
/// the hub package id, and the receiver stages that push at
/// `staging/<hub_package_id>` — byte-for-byte the path a hub-uuid-keyed swarm
/// staging dir would use. A swarm fetch racing a stray push announce for the same
/// package would then have two writers in one directory. This orchestrator-side
/// dir keeps the two paths disjoint; it sits next to `collab_serve`/`collab_pub`
/// in the same family.
const SWARM_STAGING_DIR: &str = "collab_swarm";

/// Session-scoped verdicts: package ids whose swarm fetch was tried and failed,
/// so the rest of this process goes straight to the sequential fallback.
///
/// The carrier is a module static for the same reason
/// [`PENDING_DOWNLOAD_FAILURES`] is one: this is process-lifetime scratch state
/// with no owner in the data model (there is no per-session struct on the
/// download path — `ServiceContext` is the app's service registry, and
/// `SyncRuntime` is the receive-side transport holder; neither should grow a
/// collab-download cache), and a static keeps every signature and both host
/// wrappers untouched. Cleared only by restarting the app, which is exactly the
/// spec's "one cheap failed round per package per app run" (§3.2).
static SWARM_UNFIT: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> =
    std::sync::OnceLock::new();

fn swarm_unfit() -> &'static std::sync::Mutex<HashSet<String>> {
    SWARM_UNFIT.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

/// Record that `package_id` cannot be swarm-fetched for the rest of the session.
/// Poison-tolerant: a failed lock only costs one more wasted attempt, never a
/// panic. Who may call this is decided by [`cache_swarm_unfit`].
fn mark_swarm_unfit(package_id: &str) {
    if let Ok(mut set) = swarm_unfit().lock() {
        set.insert(package_id.to_string());
    }
}

/// Which stage of a swarm attempt failed — the discrimination the verdict needs
/// (F6). Deliberately coarse: these are what the call site can know for a
/// fact, without reading error strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwarmStage {
    /// Phase 1 of `fetch_collection_multi`: the hash-seq + collection meta.
    /// Nothing was fetched at all — no provider served even that, or the
    /// transport refused the fan-out outright. Succeeding here proves the
    /// announced `root_hash` really is a collection the mesh understands.
    FetchMeta,
    /// Phase 2 of `fetch_collection_multi` (the per-child fan-out) or later —
    /// i.e. a failure AFTER phase 1 already proved the hash is real. The
    /// assignment loop's own progress deadline and backoff ladder can fail
    /// this phase with no error at all (a peer that dribbles bytes forever, or
    /// one that goes silent) — conditions that change by themselves as the
    /// swarm's holders come and go, unlike a hash the mesh will never
    /// resolve. Wave 1 final review item 1: this used to be folded into
    /// `Fetch` and cached whenever the fallback delivered, which switched the
    /// swarm off for the rest of the session on nothing more than a few
    /// stalls.
    FetchChildren,
    /// The bytes arrived and the ingest rejected frames.
    Ingest,
}

/// One failed swarm attempt: the stage plus the error the caller logs.
struct SwarmAttemptError {
    stage: SwarmStage,
    error: anyhow::Error,
}

impl SwarmAttemptError {
    /// Builds the fetch-side variant, reading WHICH phase failed off the
    /// error itself: `fetch_collection_multi` marks the per-child fan-out's
    /// own failure with [`crate::sharing::types::FetchChildrenFault`]
    /// (mirroring how `LocalFault` marks a local-disk failure for the
    /// receiver). An unmarked error reads as `FetchMeta` — true for a real
    /// phase-1 failure, and, deliberately unaddressed here, also for the rare
    /// local-bookkeeping failure AFTER a successful fan-out (tagging,
    /// exporting): that keeps the exact pre-Wave-1-review treatment for a
    /// case this review item never named, rather than guessing at a second
    /// marker for it.
    fn fetch(error: anyhow::Error) -> Self {
        let stage = if crate::sharing::types::is_fetch_children_fault(&error) {
            SwarmStage::FetchChildren
        } else {
            SwarmStage::FetchMeta
        };
        SwarmAttemptError { stage, error }
    }

    fn ingest(error: anyhow::Error) -> Self {
        SwarmAttemptError {
            stage: SwarmStage::Ingest,
            error,
        }
    }
}

/// May this session cache a swarm-unfit verdict for the attempt that just
/// failed? Spec §3.2 scopes the cache to LEGACY DISCRIMINATION — "one cheap
/// failed round per package per app run" for an announcement whose `root_hash` is
/// a pre-D3 manifest identifier — and marking every failure class blinds the
/// swarm to conditions that change by themselves (F6).
///
/// * `transport_swarm_capable = false` — no bound node, so `fetch_collection_multi`
///   is the trait's bail. Nothing on the network is involved; every later attempt
///   in this process gets the same answer. Cacheable.
/// * [`SwarmStage::FetchMeta`] with a capable transport — no provider served even
///   the collection meta. A dead swarm (holders offline) and a legacy hash
///   (holders alive, nobody holds THAT hash) are indistinguishable here… until the
///   sequential fallback answers it: a fallback that DELIVERED proves a holder was
///   alive and served the whole package, so what the swarm could not resolve is
///   the hash, not reachability. Cacheable only in that case; a whole-swarm
///   failure stays retryable, because 20 minutes later those holders may be back.
/// * [`SwarmStage::FetchChildren`] — phase 1 already proved the hash is real, so
///   NOTHING here is a statement about the hash, whatever the fallback does next.
///   The assignment loop's stall ceiling and backoff ladder fail this phase with
///   no error at all on conditions that change by themselves (a trickling peer,
///   a momentary drop) — caching THOSE off `fallback_delivered` is exactly the
///   over-caching Wave 1's final review caught (item 1). Never cacheable.
/// * [`SwarmStage::Ingest`] — the fetch worked. A per-frame rejection can be a
///   transient landing fault and says nothing about the swarm. Never cacheable.
fn cache_swarm_unfit(
    stage: SwarmStage,
    transport_swarm_capable: bool,
    fallback_delivered: bool,
) -> bool {
    if !transport_swarm_capable {
        return true;
    }
    match stage {
        SwarmStage::FetchMeta => fallback_delivered,
        SwarmStage::FetchChildren => false,
        SwarmStage::Ingest => false,
    }
}

/// Apply [`cache_swarm_unfit`] to the attempt this download made (if any), once
/// the sequential fallback's outcome is known. A no-op when no swarm attempt was
/// made or the verdict is retryable.
fn record_swarm_verdict(
    package_id: &str,
    attempt: Option<SwarmStage>,
    transport_swarm_capable: bool,
    fallback_delivered: bool,
) {
    let Some(stage) = attempt else {
        return;
    };
    if cache_swarm_unfit(stage, transport_swarm_capable, fallback_delivered) {
        mark_swarm_unfit(package_id);
        tracing::info!(
            package_id,
            stage = ?stage,
            fallback_delivered,
            "swarm marked unfit for the rest of the session"
        );
    } else {
        tracing::debug!(
            package_id,
            stage = ?stage,
            fallback_delivered,
            "swarm failure is retryable; no session verdict cached"
        );
    }
}

/// A snapshot of the session's swarm-unfit verdicts (the plan takes it by
/// reference so it stays a pure function). Poison ⇒ an empty set: retrying a
/// swarm fetch is the safe degradation, never a wrong download.
fn swarm_unfit_snapshot() -> HashSet<String> {
    swarm_unfit()
        .lock()
        .map(|s| s.clone())
        .unwrap_or_else(|_| HashSet::new())
}

/// Could `s` be an iroh collection hash? Shape only — 64 hex characters, which
/// is also exactly what the hub validates.
///
/// This can never be the real discrimination: a pre-D3 announcement's
/// `root_hash` is a BLAKE3 of the manifest bytes, which passes this same shape
/// test and no provider's blob store contains (spec §3.2). Legacy is told apart
/// by TRYING and failing, then caching the verdict — this gate only keeps
/// obvious non-hashes (the `"rh"`-style placeholder rows) off the wire.
fn looks_like_collection_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Decide whether to attempt a swarm fetch, and against whom — the pure,
/// unit-tested half of the swarm path.
///
/// `Some(providers)` = the holder set minus this device, each with its
/// self-reported relay hint preserved (the dial-hint builder needs it per
/// provider). `None` = go straight to the sequential fallback, for any of:
/// the root hash cannot be a collection hash / the package is not swarm-eligible
/// (`root_hash_is_swarm_capable`, which the caller also ANDs with the published
/// state — spec §5 keeps a pending package on the authz-checked fallback), the
/// package already earned a swarm-unfit verdict this session, or nobody but me
/// holds it.
///
/// The self-filter is deliberately duplicated here (the caller's `holders` are
/// already self-filtered for the sequential loop): the plan must be decidable on
/// its own inputs, and a provider set containing our own node would make the
/// downloader dial itself.
fn swarm_fetch_plan(
    holders: &[(NodeId, String, Option<String>)],
    own_node: NodeId,
    root_hash_is_swarm_capable: bool,
    swarm_unfit: &HashSet<String>,
    package_id: &str,
) -> Option<Vec<(NodeId, Option<String>)>> {
    if !root_hash_is_swarm_capable || swarm_unfit.contains(package_id) {
        return None;
    }
    let providers: Vec<(NodeId, Option<String>)> = holders
        .iter()
        .filter(|(n, _, _)| *n != own_node)
        .map(|(n, _, relay)| (*n, relay.clone()))
        .collect();
    if providers.is_empty() {
        return None;
    }
    Some(providers)
}

/// Packages whose pull is running RIGHT NOW, keyed by the hub package uuid — the
/// exclusion that makes overlapping pulls of one package impossible (F1).
///
/// The concurrency was real and had three sources: "Sync now" spawned a
/// pass per press with no de-duplication, the package-era need diff
/// re-admitted `downloading` AND `failed` rows on every 20-minute pass, and
/// the UI's Retry button calls the command directly. (Since wave 2 Task 9 the
/// pass replicates frames, not packages; only the command still pulls a
/// package, until Task 12 retires it.) Everything a pull touches is keyed by the
/// package id alone — the swarm staging dir (`collab_swarm/<package_id>`), the
/// collection tag `release` reclaims, and the row's `local_status` — so two
/// pulls of one package are two writers on one set of resources: the faster
/// one's [`remove_swarm_staging`] + `release` yank the dir and the bytes out
/// from under the slower one's export/ingest, whose failure then re-arms
/// `downloading` over the winner's `complete` and finally stamps `failed` on a
/// package that is fully landed and seeded.
///
/// A module static for the same reason [`SWARM_UNFIT`] is one: process-lifetime
/// scratch state with no owner in the data model, and it keeps every signature
/// and both host wrappers untouched. Cross-PROCESS overlap (desktop + web on one
/// catalog) is out of its reach, which is why the two status guards below
/// ([`rearm_for_fallback`], [`set_download_failed`]) stand on their own.
static IN_FLIGHT_PACKAGE_PULLS: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> =
    std::sync::OnceLock::new();

fn in_flight_package_pulls() -> &'static std::sync::Mutex<HashSet<String>> {
    IN_FLIGHT_PACKAGE_PULLS.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

/// An RAII claim on one package's pull. The entry is released on EVERY exit —
/// early return, `?`, or a panic in the pull — because a leaked claim would make
/// that package undownloadable for the rest of the process.
struct PackagePullClaim(String);

impl PackagePullClaim {
    /// `None` when another pull of `package_id` already holds the claim.
    ///
    /// Poison-tolerant in the permissive direction: a poisoned lock grants the
    /// claim, because the guard is protection against a race and refusing every
    /// download after one unrelated panic would be the worse failure.
    fn acquire(package_id: &str) -> Option<Self> {
        match in_flight_package_pulls().lock() {
            Ok(mut set) => set
                .insert(package_id.to_string())
                .then(|| PackagePullClaim(package_id.to_string())),
            Err(_) => Some(PackagePullClaim(package_id.to_string())),
        }
    }
}

impl Drop for PackagePullClaim {
    fn drop(&mut self) {
        if let Ok(mut set) = in_flight_package_pulls().lock() {
            set.remove(&self.0);
        }
    }
}

/// Д6 explicit download of a project package: role-gate, poll the hub for the
/// package's current holders, then try each holder sequentially — attach a dial
/// hint (audit B4), `request_project`, and wait for the served package to ingest
/// (the Task-5 receiver arm flips `local_status` to `complete`). The first
/// success reports-have to the hub and returns; all holders exhausted lands
/// `failed`.
///
/// **Dial hints are mandatory on the real network (audit B4).** A bare
/// account-resolved holder node id is undialable: without a
/// [`pairing::peer_addr_with_relays`] hint attached via the shared node's
/// [`add_peer`](crate::sharing::iroh::node::SharedIrohNode::add_peer),
/// `request_project` falls back to a hint-less `EndpointAddr` and fails with "No
/// addressing information available". The bound node is read off
/// [`ServiceContext::iroh_node`]; loopback tests bypass dialing (in-process
/// mailbox routing) and never bind a node, so it is `None` there and the hint
/// step is skipped.
///
/// Runs on a command-spawned task (Task 11): the terminal `local_status` +
/// `sync-finished` event carry the outcome, so returning `Ok` on an exhausted
/// attempt is normal, not an error.
///
/// **D3 §3.1: a swarm attempt runs first.** When the announcement carries a
/// usable collection hash and someone else holds the package, one
/// [`fetch_collection_multi`](SharingTransport::fetch_collection_multi) pulls it
/// from EVERY holder at once (split per child blob, byte-level failover) and, on
/// success, ingests + reports-have and returns. Any failure falls through to the
/// sequential loop below IN THE SAME CALL — the user pressed Download, not "try
/// again in 20 minutes" — and only the failure classes that cannot change this
/// session cache a swarm-unfit verdict ([`cache_swarm_unfit`], F6: a legacy
/// `root_hash` no provider serves, or a transport that cannot fan out; never a
/// dead swarm or a rejected ingest). The sequential loop itself is untouched by
/// D3.
///
/// `emitter` carries the `project-download-progress` events of the swarm path
/// ([`ProjectDownloadProgress`]); the auto-replication worker (D3 T5) passes the
/// host emitter too, so a background pull is as visible as a pressed Download.
/// `None` (tests) is a silent run, never a different transfer.
///
/// **One pull per package at a time** ([`IN_FLIGHT_PACKAGE_PULLS`], F1). A call
/// that arrives while this package is already being pulled returns `Ok(())`
/// after a log line rather than starting a second, resource-sharing attempt: an
/// in-flight pull IS the requested work, and the caller's contract (the terminal
/// `local_status` carries the outcome, not the return value) holds either way.
/// The claim spans the swarm attempt AND the sequential fallback.
pub async fn download_project_package(
    ctx: &ServiceContext,
    sync: &crate::sync::SyncRuntime,
    project_id: &str,
    package_id: &str,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<(), ApiError> {
    let _claim = match PackagePullClaim::acquire(package_id) {
        Some(claim) => claim,
        None => {
            tracing::info!(
                project_id,
                package_id,
                "download skipped: a pull of this package is already running"
            );
            return Ok(());
        }
    };
    let dirs = crate::api::sync::sync_dirs(ctx)?;
    let db_path = dirs.db_path.clone();

    // ── Role guard (fail-closed) ─────────────────────────────────────────────
    // Only a send_receive member or the coordinator may pull. Own membership is
    // resolved by matching THIS device's node id in the cached snapshot (the
    // account id is not part of the snapshot keying). The key is IDENTITY, so it
    // is read from the identity dir — never from the relocatable working dir.
    let own_node = DeviceKey::load_or_create(&device_key_path(&dirs.identity_dir))
        .map_err(|e| ApiError::Internal(format!("device key: {e:#}")))?
        .node_id();
    {
        let db = db(ctx)?;
        let conn = db.conn();
        let allowed = match crate::collab::authz::member_for_node(&conn, project_id, &own_node) {
            Some(m) => m.coordinator || m.data_role == "send_receive",
            None => false,
        };
        if !allowed {
            // M2/F6: a spawned download task's Err never reaches the UI, whose
            // Receive-tab busy state clears only when local_status leaves "none".
            // Flip an existing row to failed so the spinner doesn't hang. Status
            // only (no `downloadFailed` notification) — a role rejection is a
            // config issue, not an attempted transfer; the UPDATE no-ops when the
            // row doesn't exist yet.
            if let Err(e) = set_local_status(&conn, package_id, "failed") {
                tracing::warn!(package_id, error = %format!("{e}"), "download: role-guard set failed status errored");
            }
            return Err(ApiError::Invalid(format!(
                "this device's role in project {project_id} does not permit downloading packages"
            )));
        }
    }

    // ── Mark downloading (before any network I/O, so the UI reflects intent). ─
    {
        let db = db(ctx)?;
        let conn = db.conn();
        set_local_status(&conn, package_id, "downloading")?;
    }

    // ── Fresh poll: upsert the row AND read the package's current holders. ────
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        set_download_failed(ctx, project_id, package_id, None);
        return Err(ApiError::SignedOut(
            "Sign in to download a project package.".into(),
        ));
    };
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    #[allow(deprecated)] // collab v3: `list_announcements` removed in wave 2 Task 11
    let anns = match client.list_announcements(&token, project_id).await {
        Ok(a) => a,
        Err(e) => {
            set_download_failed(ctx, project_id, package_id, None);
            return Err(client_err(e));
        }
    };
    {
        let db = db(ctx)?;
        let conn = db.conn();
        apply_announcements(&conn, project_id, &anns)?;
    }

    // The freshly-polled announcement for this package (keyed by hub uuid).
    let Some(ann) = anns.into_iter().find(|a| a.package_id == package_id) else {
        tracing::warn!(
            project_id,
            package_id,
            "download: hub no longer lists this package"
        );
        set_download_failed(ctx, project_id, package_id, None);
        return Ok(());
    };

    // Candidate holders (exclude MY own node — I can't serve myself). Each
    // carries the holder's self-reported relay url (T7 / finding H1) so the dial
    // hint below can target the holder's REAL relay, not just our own set.
    let holders: Vec<(NodeId, String, Option<String>)> = ann
        .holders
        .iter()
        .filter_map(|h| {
            pairing::node_id_from_pubkey_b64(&h.pubkey)
                .ok()
                .map(|n| (n, h.display_name.clone(), h.relay_url.clone()))
        })
        .filter(|(n, _, _)| *n != own_node)
        .collect();

    if holders.is_empty() {
        tracing::warn!(
            project_id,
            package_id,
            "download: no other holder to pull from"
        );
        set_download_failed(ctx, project_id, package_id, None);
        return Ok(());
    }

    // request_project rides the SAME endpoint the receiver listens on (an
    // outbound send; it never touches the receiver's single-consumer stream).
    let transport = sync.transport().await.ok_or_else(|| {
        ApiError::Internal("sync transport not started; cannot request a project package".into())
    })?;
    // Dial hints ride the shared node (C1). Read the ALREADY-BOUND node off the
    // context — never bind here: a loopback test injects a transport via
    // `set_started_for_test` without binding a node, so `None` means in-process
    // routing that needs no dial hint (exactly the old `iroh_handle().is_none()`
    // case). Production always has a node bound (the receiver started it first).
    let node = ctx.iroh_node.lock().await.clone();
    // The relay map is only needed to build dial hints on the real-network path.
    let relay_urls = if node.is_some() {
        crate::api::sync::resolve_relay_mode(ctx).await?.1
    } else {
        Vec::new()
    };

    // ── D3 §3.1: the swarm attempt, BEFORE the sequential fallback ───────────
    //
    // Eligibility is deliberately coarse: the hub validates `root_hash` as 64
    // hex, which a LEGACY (pre-D3) manifest identifier satisfies too, so shape
    // can never tell a real collection hash from one — the discrimination is
    // try-then-fallback with a cached verdict (spec §3.2). The published gate is
    // spec §5: a still-pending package is only ever pulled through the
    // authz-checked fallback (a raw blob GET carries no `may_serve_package`
    // check, so the swarm path must not become a way around it).
    let swarm_capable = ann.state == "published" && looks_like_collection_hash(&ann.root_hash);
    // What a failed swarm attempt left behind, for the session verdict decided at
    // the fallback's exits below (F6): the stage that failed, plus whether the
    // transport could fan out at all (a bound node means the collab role handle;
    // without one the trait's default bails, which no retry can change).
    let mut swarm_attempt: Option<SwarmStage> = None;
    let swarm_transport_capable = node.is_some();
    if let Some(providers) = swarm_fetch_plan(
        &holders,
        own_node,
        swarm_capable,
        &swarm_unfit_snapshot(),
        package_id,
    ) {
        // The swarm rides the COLLAB role handle when a node is bound (its
        // in-flight tag then lands under `in-flight/collab/pkg/…`, outside the
        // prefix the receiver's B7 orphan sweep reclaims — see the role note on
        // `SharedIrohNode::role_fetch_multi`). With no node bound (loopback
        // tests) it rides the started transport, whose default
        // `fetch_collection_multi` bails — an explicit fallback, not a silent
        // degradation to one provider.
        let swarm_transport: Arc<dyn SharingTransport> = match &node {
            Some(n) => n.handle(Role::Collab),
            None => Arc::clone(&transport),
        };
        let sources = providers.len();
        match try_swarm_download(
            sync,
            &swarm_transport,
            node.as_ref(),
            &relay_urls,
            &providers,
            project_id,
            package_id,
            &ann.root_hash,
            ann.byte_size.max(0) as u64,
            // Swarm staging (`collab_swarm/<pkg>`) is DATA → working dir.
            &dirs.working_dir,
            &db_path,
            emitter.as_deref(),
        )
        .await
        {
            Ok(()) => {
                // Seed BEFORE reporting have (D3 §3.4/T4): `report_have` is what
                // puts this device on the hub's holder list, so advertising ahead
                // of a servable seed would publish a phantom every other member's
                // fetch has to fail over. Best-effort — a failed seed still
                // reports have, because the package is still servable through the
                // on-demand `handle_project_request` path.
                seed_ingested_package(ctx, package_id).await;
                // No ack on this path (spec §3.1.4): an ack settles a SENDER's
                // outbound row, and no holder enqueued one — nobody was asked to
                // serve. `report_have` is this path's completion signal, exactly
                // as it is the sequential loop's.
                #[allow(deprecated)] // collab v3: `report_have` removed in wave 2 Task 11
                if let Err(e) = client.report_have(&token, &ann.id).await {
                    tracing::warn!(announcement_id = %ann.id, error = %format!("{e}"), "swarm download: report_have failed after ingest");
                }
                tracing::info!(project_id, package_id, sources, "swarm download complete");
                return Ok(());
            }
            Err(attempt) => {
                // The session verdict is NOT decided here (F6): a dead swarm and
                // a legacy hash both look like "nobody served it", and only the
                // fallback's outcome tells them apart — see `cache_swarm_unfit`,
                // applied at both exits below.
                swarm_attempt = Some(attempt.stage);
                tracing::warn!(
                    project_id,
                    package_id,
                    sources,
                    stage = ?attempt.stage,
                    error = %format!("{:#}", attempt.error),
                    "swarm download failed; falling back to the sequential holder loop"
                );
                // Re-arm the row for the fallback ([`rearm_for_fallback`] — a
                // partial swarm ingest wrote `failed`, which the fallback's
                // `wait_for_local_complete` would read as a stale verdict), but
                // never over a `complete` another writer landed meanwhile (F1).
                // Best-effort: a write failure only costs the fallback, never the
                // caller's result.
                {
                    let db = db(ctx)?;
                    let conn = db.conn();
                    match rearm_for_fallback(&conn, package_id) {
                        Ok(true) => {}
                        Ok(false) => tracing::info!(
                            package_id,
                            "swarm download: package already complete; fallback not re-armed"
                        ),
                        Err(e) => {
                            tracing::warn!(package_id, error = %format!("{e}"), "swarm download: re-arm status for the fallback errored")
                        }
                    }
                }
            }
        }
    }

    // Per-holder probe failure classes (Task 9), surfaced in the terminal
    // `downloadFailed` detail if every holder is exhausted — an operator then sees
    // WHY each holder was skipped (offline / refused / relay_unreachable), not just
    // "download failed".
    let mut probe_failures: Vec<String> = Vec::new();

    for (holder_node, holder_name, holder_relay) in &holders {
        // Attach this holder's dial hint before the request, via the shared node's
        // `add_peer`. Prefer the holder's OWN reported relay (T7 / finding H1),
        // falling back to our resolved relay set when the hub served none. This is
        // CROSS-ACCOUNT (a collab holder may be in a different account), so
        // `peer_dial_addr(cross_account = true)` carries the relay ONLY and never
        // any direct addrs (S1). A loopback runtime (no bound node) routes
        // in-process and needs no hint.
        if let Some(node) = &node {
            let reported = holder_relay
                .as_ref()
                .map(|url| crate::account::EndpointAddrReport {
                    home_relay_url: Some(url.clone()),
                    direct_addrs: Vec::new(),
                    reported_at: None,
                });
            match pairing::peer_dial_addr(*holder_node, reported.as_ref(), &relay_urls, true) {
                Ok(addr) => node.add_peer(addr),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), holder = %holder_name, "download: dial hint build failed; skipping holder");
                    continue;
                }
            }

            // Short reachability probe (Task 9): before committing to the 90s blob
            // poll, a 5s control-connect probe skips a dead/refusing holder fast and
            // records why. A relay hint is present when the holder reported one or
            // we resolved our own relay set (the dial hint above carried it).
            let has_relay_hint = holder_relay.is_some() || !relay_urls.is_empty();
            if let Err(class) = node
                .probe_holder(*holder_node, has_relay_hint, HOLDER_PROBE_TIMEOUT)
                .await
            {
                tracing::warn!(
                    project_id,
                    package_id,
                    holder = %holder_name,
                    class = class.as_str(),
                    "download: holder probe failed; next holder"
                );
                probe_failures.push(format!("{}: {}", holder_name, class.as_str()));
                continue;
            }
        }

        if let Err(e) = transport
            .request_project(*holder_node, project_id, package_id)
            .await
        {
            tracing::warn!(error = %format!("{e:#}"), holder = %holder_name, "download: request_project failed; next holder");
            continue;
        }
        tracing::info!(project_id, package_id, holder = %holder_name, "download: requested serve; awaiting ingest");

        // Wait for the receiver to flip local_status to complete (Task 5 ingest).
        if wait_for_local_complete(ctx, package_id).await {
            // A holder just served the whole package, so any swarm fetch that
            // failed above failed on the HASH, not on reachability (F6).
            record_swarm_verdict(package_id, swarm_attempt, swarm_transport_capable, true);
            // No seed call here (D3 T4): on this path the RECEIVER ingested, and
            // its `on_project_ingested` hook already seeds before its own
            // report-have. That hook runs on a spawned task, so this loop's
            // report-have can win the race — which is fine and is exactly the
            // pre-D3 semantics: a landed package is servable through
            // `handle_project_request` whether or not the seed has landed yet.
            // Report-have (device bearer) so the hub adds us to the swarm.
            #[allow(deprecated)] // collab v3: `report_have` removed in wave 2 Task 11
            if let Err(e) = client.report_have(&token, &ann.id).await {
                tracing::warn!(announcement_id = %ann.id, error = %format!("{e}"), "download: report_have failed after ingest");
            }
            tracing::info!(project_id, package_id, holder = %holder_name, "download complete");
            return Ok(());
        }
        tracing::warn!(project_id, package_id, holder = %holder_name, "download: holder did not deliver in time; next holder");
    }

    // Every holder exhausted — nobody delivered by ANY path, so a swarm failure
    // above is a dead swarm, not a legacy hash: no session verdict unless the
    // transport itself cannot fan out (F6).
    record_swarm_verdict(package_id, swarm_attempt, swarm_transport_capable, false);

    // Carry the per-holder probe classes (Task 9) into the `downloadFailed`
    // detail so the notification names why each holder was skipped.
    let detail = if probe_failures.is_empty() {
        None
    } else {
        Some(format!(
            "no holder delivered — {}",
            probe_failures.join("; ")
        ))
    };
    set_download_failed(ctx, project_id, package_id, detail);
    tracing::warn!(
        project_id,
        package_id,
        holders = holders.len(),
        "download failed: no holder delivered"
    );
    Ok(())
}

/// One swarm attempt for `package_id` (D3 §3.1): dial hints → receive permit →
/// [`fetch_collection_multi`](SharingTransport::fetch_collection_multi) across
/// every provider → ingest. `Ok(())` means the package is locally complete; an
/// `Err` is the caller's signal to fall back, carrying the [`SwarmStage`] the
/// session verdict is decided from ([`cache_swarm_unfit`], F6).
///
/// Status transitions are the fallback's, unchanged: the row is already
/// `downloading` (set by the caller before any network I/O), and
/// [`ingest_project_package`](crate::sync::ingest_project_package) is the ONE
/// writer of the terminal `complete`/`failed` — the same write the sequential
/// path waits on in `wait_for_local_complete`. Nothing here second-guesses it.
#[allow(clippy::too_many_arguments)]
async fn try_swarm_download(
    sync: &crate::sync::SyncRuntime,
    swarm_transport: &Arc<dyn SharingTransport>,
    node: Option<&Arc<crate::sharing::iroh::node::SharedIrohNode>>,
    relay_urls: &[String],
    providers: &[(NodeId, Option<String>)],
    project_id: &str,
    package_id: &str,
    root_hash: &str,
    byte_size: u64,
    sync_dir: &Path,
    db_path: &Path,
    emitter: Option<&dyn ProgressEmitter>,
) -> Result<(), SwarmAttemptError> {
    // The hub id is peer-minted; guard it before it becomes a path segment (C1),
    // exactly as `reconstruct_serve_dir` does.
    crate::package::validate_package_id(package_id)
        .with_context(|| format!("reject unsafe collab package id {package_id}"))
        .map_err(SwarmAttemptError::fetch)?;
    let staging = sync_dir.join(SWARM_STAGING_DIR).join(package_id);

    // Dial hints for EVERY provider up front (the sequential loop attaches one
    // per holder inside its loop; the swarm has no loop). Same rule, verbatim:
    // prefer the holder's own reported relay, fall back to our resolved set, and
    // `cross_account = true` so the hint carries the relay ONLY, never direct
    // addrs (S1). A hint that cannot be built costs that provider its dial, not
    // the fetch — the others still serve.
    if let Some(node) = node {
        for (holder, relay) in providers {
            let reported = relay
                .as_ref()
                .map(|url| crate::account::EndpointAddrReport {
                    home_relay_url: Some(url.clone()),
                    direct_addrs: Vec::new(),
                    reported_at: None,
                });
            match pairing::peer_dial_addr(*holder, reported.as_ref(), relay_urls, true) {
                Ok(addr) => node.add_peer(addr),
                Err(e) => tracing::warn!(
                    error = %format!("{e:#}"),
                    holder = %node_id_hex(holder),
                    "swarm download: dial hint build failed for one provider"
                ),
            }
        }
    }

    // Fairness (spec §3.1.5): a project pull is a receive like any other, so it
    // takes ONE `ReceiveGate` permit for the whole fetch + ingest. A receiver
    // that has not started has no gate — proceed WITHOUT a permit: the gate is a
    // fairness device between concurrent receives, not a correctness gate, and a
    // not-started receiver means there are no competing receives to be fair to.
    // Dropped on return, so the sequential fallback never runs holding it.
    let _receive_permit = match sync.inbound_control().await {
        Some(control) => Some(control.receive_gate.acquire().await),
        None => {
            tracing::debug!(
                package_id,
                "swarm download: no receiver started; no receive permit"
            );
            None
        }
    };

    let sources = providers.len();
    if let Some(emitter) = emitter {
        crate::events::emit_event(
            emitter,
            "project-download-progress",
            &ProjectDownloadProgress {
                project_id: project_id.to_string(),
                package_id: package_id.to_string(),
                sources,
                stage: "fetching".to_string(),
            },
        );
    }
    tracing::info!(
        project_id,
        package_id,
        sources,
        root_hash,
        "swarm download attempt"
    );

    // Per-provider journal. There is no collab-side `sync_events` journal to
    // write to — that table is keyed by a transfer's `batch_key` and a project
    // pull has no `sync_inbound` row — so the journal IS `tracing`, exactly like
    // the sequential loop's per-holder lines. Debug level: the stream is a
    // lossy sample (see `ProviderTelemetrySink`), and a `Failed` is a provider
    // switch, not an error.
    let telemetry: ProviderTelemetrySink = {
        let package_id = package_id.to_string();
        Arc::new(move |ev| match ev {
            ProviderEvent::Trying(id) => {
                tracing::debug!(package_id = %package_id, holder = %node_id_hex(&id), "swarm download: provider tried")
            }
            ProviderEvent::Failed(id) => {
                tracing::debug!(package_id = %package_id, holder = %node_id_hex(&id), "swarm download: provider failed; switching")
            }
        })
    };

    let fetch = swarm_transport
        .fetch_collection_multi(
            providers.iter().map(|(n, _)| *n).collect(),
            root_hash,
            byte_size,
            &staging,
            noop_fetch_sink(),
            telemetry,
        )
        .await
        .with_context(|| format!("swarm fetch collection {root_hash} for package {package_id}"));

    let emit_done = |emitter: Option<&dyn ProgressEmitter>| {
        if let Some(emitter) = emitter {
            crate::events::emit_event(
                emitter,
                "project-download-progress",
                &ProjectDownloadProgress {
                    project_id: project_id.to_string(),
                    package_id: package_id.to_string(),
                    sources,
                    stage: "done".to_string(),
                },
            );
        }
    };

    if let Err(e) = fetch {
        // Remove the staging dir on failure too: the verified partial BYTES live
        // in the blob store under the in-flight tag (which the fetch keeps on
        // every non-success exit, so a retry resumes across any holder), and a
        // half-exported directory would only be re-exported wholesale by the next
        // attempt. Nothing resumable is thrown away here.
        remove_swarm_staging(&staging);
        emit_done(emitter);
        return Err(SwarmAttemptError::fetch(e));
    }

    // Ingest on a blocking thread over its OWN catalog connection (W2's
    // `IngestConn::Shared` per-frame locking — never the app's `db(ctx)` guard,
    // which would be held for the whole multi-GB package).
    //
    // Racing a stray push announce for the same package (the receiver's
    // `handle_project_announce` arm) is SAFE but not free. Sequentially it is a
    // no-op: `ingest_project_package` resolves a same-(project, publisher, uuid)
    // record with identical xxh3 as a Duplicate BEFORE touching the payload, so
    // the loser lands nothing, inserts no contribution row, and only re-writes
    // the same `local_status`. Truly CONCURRENTLY the two arms hold different
    // `CatalogSyncStore` connections, so the per-frame guard does not span them
    // and both can pass the duplicate check for one frame — costing a second
    // landed copy + contribution row for that uuid, never a corrupt one. Both
    // arms take a `ReceiveGate` permit from the SAME gate, so that window only
    // opens at `sync.max_concurrent_receives >= 2`. Named, not fixed here:
    // closing it means one shared ingest lock across the receiver and this
    // orchestrator, and the receiver arm is the untouched fallback path.
    let outcome = {
        let staging = staging.clone();
        let project_id = project_id.to_string();
        let package_id = package_id.to_string();
        let db_path = db_path.to_path_buf();
        // The serving "peer" recorded in `sync_history`: a swarm fetch has no
        // single serving device, so the literal names the PATH instead of
        // pretending one holder delivered it. History metadata only — the landing
        // slug is hub-anchored (Д5), and the Transfers history renders an unknown
        // peer string verbatim (`shortPeer`), never parses it as hex.
        tokio::task::spawn_blocking(move || -> Result<crate::sync::ProjectIngestOutcome> {
            let store = CatalogSyncStore::open(&db_path)
                .with_context(|| format!("open catalog sync store {}", db_path.display()))?;
            crate::sync::ingest_project_package(
                crate::sync::IngestConn::Shared(&store),
                &staging,
                &project_id,
                &package_id,
                "swarm",
            )
        })
        .await
        .context("swarm project ingest join")
        .map_err(SwarmAttemptError::ingest)?
    };
    remove_swarm_staging(&staging);
    emit_done(emitter);
    let outcome = outcome.map_err(SwarmAttemptError::ingest)?;

    // Drop the fetched blobs (best-effort, idempotent) — the payloads are landed
    // contributions now, and the collection tag would otherwise keep a second
    // full copy in the blob store forever. The tag is keyed by the canonical
    // lowercase hex of the collection hash (`role_fetch_multi`), and `release`
    // reclaims its `in-flight/` derivative too.
    if let Err(e) = swarm_transport
        .release(&PackageId(root_hash.to_lowercase()))
        .await
    {
        tracing::warn!(package_id, root_hash, error = %format!("{e:#}"), "swarm download: blob release failed");
    }

    if !outcome.failed.is_empty() {
        // Ingest already wrote `failed`. Report it as a swarm failure so the
        // sequential fallback gets its turn — a per-frame reject can be a
        // transient landing fault, and the fallback re-delivers the same frames
        // through an ingest that is idempotent per (project, publisher, uuid).
        // Ingest-stage, so it never earns a session verdict (F6): the fetch
        // itself worked, which is all the swarm was ever asked to do.
        return Err(SwarmAttemptError::ingest(anyhow!(
            "swarm ingest rejected {} of {} frames",
            outcome.failed.len(),
            outcome.failed.len() + outcome.ok_count
        )));
    }
    tracing::info!(
        project_id,
        package_id,
        ok = outcome.ok_count,
        sources,
        "swarm download ingested"
    );
    Ok(())
}

/// Re-arm a row for the sequential fallback after a failed swarm attempt —
/// UNLESS it is already `complete` (F1). Returns whether it re-armed.
///
/// The re-arm exists because a PARTIAL swarm ingest writes `failed`, and
/// [`wait_for_local_complete`] returns false the instant it reads `failed` — so
/// without it the fallback would skip every holder in milliseconds on a stale
/// verdict instead of waiting for the serve it just requested.
///
/// The `complete` exclusion exists because this attempt is not the only writer:
/// the receiver's push arm (a stray announce for the same package) can land it
/// while the swarm fetch is in flight, and a second app process shares the
/// catalog. Re-arming over a landed package would put a fully ingested, seeded
/// package back into `downloading` and hand the terminal `failed` below the last
/// word.
fn rearm_for_fallback(conn: &rusqlite::Connection, package_id: &str) -> Result<bool> {
    if let Some(row) = get_package(conn, package_id)? {
        if row.local_status == "complete" {
            return Ok(false);
        }
    }
    set_local_status(conn, package_id, "downloading")?;
    Ok(true)
}

/// Best-effort removal of a swarm staging dir. Missing is normal (a fetch that
/// failed before the export step never created it); anything else is logged,
/// never swallowed, and never fails the download.
fn remove_swarm_staging(staging: &Path) {
    match std::fs::remove_dir_all(staging) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(path = %staging.display(), error = %e, "swarm download: staging cleanup failed")
        }
    }
}

/// Best-effort `set_local_status("failed")` for a genuine download attempt, plus
/// an enqueued `downloadFailed` change so the next `refresh_all_project_packages`
/// surfaces it (F3 — the download runs on a spawned task that can't `notify()`
/// itself). Logged, never masks the caller's own error/return. `detail` (Task 9)
/// carries the per-holder probe classes for the notification; `None` when the
/// failure had no per-holder classification (signed out, hub blip, no holders).
///
/// **Never over a `complete` row** (F1): this attempt is not the only writer —
/// the receiver's push arm, a second app process on the same catalog, or (before
/// [`IN_FLIGHT_PACKAGE_PULLS`]) a concurrent pass can land the package while
/// this attempt is still walking holders. Stamping `failed` on a fully landed,
/// seeded package would make the row lie, re-admit it to the need diff forever,
/// and raise a `downloadFailed` notification for work that succeeded — so the
/// buffered change is suppressed with the status write.
fn set_download_failed(
    ctx: &ServiceContext,
    project_id: &str,
    package_id: &str,
    detail: Option<String>,
) {
    if let Ok(db) = db(ctx) {
        let conn = db.conn();
        match get_package(&conn, package_id) {
            Ok(Some(row)) if row.local_status == "complete" => {
                tracing::info!(
                    project_id,
                    package_id,
                    "download attempt failed but the package is already complete; status kept"
                );
                return;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(package_id, error = %format!("{e}"), "download: reading the row before failing it errored")
            }
        }
        if let Err(e) = set_local_status(&conn, package_id, "failed") {
            tracing::warn!(package_id, error = %format!("{e}"), "download: set failed status errored");
        }
    }
    push_download_failure(project_id, package_id, detail);
}

/// Process-local buffer of package state changes observed OFF the UI's refresh
/// path, so the next [`refresh_all_project_packages`] reports each of them
/// exactly once. Two producers:
///
/// * `downloadFailed` from a spawned `download_project_package` task, which
///   returns into a task, not the UI (F3);
/// * the diffs the D3 auto-replication worker's own announcement refresh
///   consumed — `apply_announcements` diffs against the DB, so once the worker
///   has upserted a row a later UI poll sees it as KNOWN and would raise
///   nothing. Without this buffer, auto-replication would silently swallow the
///   `newPackage` / `approved` / `rejected` notifications for exactly the
///   projects it keeps most current.
static PENDING_PACKAGE_CHANGES: std::sync::OnceLock<std::sync::Mutex<Vec<PackageStateChange>>> =
    std::sync::OnceLock::new();

fn pending_package_changes() -> &'static std::sync::Mutex<Vec<PackageStateChange>> {
    PENDING_PACKAGE_CHANGES.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Enqueue a `downloadFailed` change (F3). Poison-tolerant: a failed lock only
/// means the change isn't surfaced this cycle, never a panic. `detail` (Task 9)
/// is the per-holder probe classification summary, or `None`.
fn push_download_failure(project_id: &str, package_id: &str, detail: Option<String>) {
    push_package_changes(vec![PackageStateChange {
        project_id: project_id.to_string(),
        package_id: package_id.to_string(),
        kind: "downloadFailed".to_string(),
        detail,
    }]);
}

/// Hard cap on the buffer (F7). Only a UI refresh drains it, so a headless host
/// or a window nobody opens for a week lets the auto-replication worker append
/// forever — every pass buffers its diffs. 200 is far more than any one refresh
/// would show; past it the OLDEST go, because a notification the user never saw
/// for an event days old is the one worth losing.
const MAX_PENDING_PACKAGE_CHANGES: usize = 200;

/// One warn per process when the cap first bites — an unbounded log line per
/// overflowing pass would be its own leak.
static PENDING_OVERFLOW_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Enqueue changes a non-UI refresh consumed (D3 T5). Poison-tolerant, same as
/// [`push_download_failure`]. Capped at [`MAX_PENDING_PACKAGE_CHANGES`],
/// drop-oldest.
fn push_package_changes(changes: Vec<PackageStateChange>) {
    if changes.is_empty() {
        return;
    }
    if let Ok(mut buf) = pending_package_changes().lock() {
        buf.extend(changes);
        if buf.len() > MAX_PENDING_PACKAGE_CHANGES {
            let dropped = buf.len() - MAX_PENDING_PACKAGE_CHANGES;
            buf.drain(..dropped);
            if !PENDING_OVERFLOW_WARNED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                tracing::warn!(
                    cap = MAX_PENDING_PACKAGE_CHANGES,
                    dropped,
                    "collab package-change buffer full; dropping the oldest entries (nothing has drained it — no project refresh since app start?)"
                );
            }
        }
    }
}

/// Drain the buffered changes exactly once (F3), keeping only those whose
/// project this device still has (F7).
///
/// A project can be left, deleted, or dropped from the hub between the buffering
/// pass and the refresh that drains it; replaying its `downloadFailed` /
/// `newPackage` would notify about a project the user cannot even open, and the
/// entry would otherwise sit in the buffer for the life of the process.
fn drain_pending_package_changes(known_projects: &HashSet<String>) -> Vec<PackageStateChange> {
    let buffered = match pending_package_changes().lock() {
        Ok(mut buf) => std::mem::take(&mut *buf),
        Err(_) => Vec::new(),
    };
    let before = buffered.len();
    let kept: Vec<PackageStateChange> = buffered
        .into_iter()
        .filter(|c| known_projects.contains(&c.project_id))
        .collect();
    if kept.len() != before {
        tracing::debug!(
            dropped = before - kept.len(),
            "buffered package changes dropped for projects this device no longer has"
        );
    }
    kept
}

/// Poll `project_packages.local_status` every [`DOWNLOAD_POLL_INTERVAL`] up to
/// [`DOWNLOAD_POLL_TIMEOUT`], returning `true` the moment it reads `complete` and
/// `false` on `failed` or timeout.
async fn wait_for_local_complete(ctx: &ServiceContext, package_id: &str) -> bool {
    let deadline = tokio::time::Instant::now() + DOWNLOAD_POLL_TIMEOUT;
    loop {
        if let Ok(db) = db(ctx) {
            let conn = db.conn();
            match get_package(&conn, package_id) {
                Ok(Some(row)) if row.local_status == "complete" => return true,
                Ok(Some(row)) if row.local_status == "failed" => return false,
                _ => {}
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(DOWNLOAD_POLL_INTERVAL).await;
    }
}

// ── Collab v3 wave 2: per-frame replication (Task 9) ──────────────────────────
//
// The pass below turns the manifest (Task 8) into files on disk and the disk
// back into holder reports:
//
// - disk truth: every landed row's file is stat'ed (and re-hashed when its
//   `size:mtime` moved); a file that is gone or edited is MISSING — its seed
//   tags are dropped and the hub stops sending peers here (spec §5.5, P8);
// - the loss guard (P14) pauses replication when "missing" looks like a
//   folder moved away rather than a frame deleted;
// - the need set (spec §5.3) is every published, accepted peer frame not on
//   disk, filtered by the project's replication policy, rarest first;
// - the fetch pulls a batch through the generalized assignment engine (P11)
//   into the collab store and lands each frame by `export_child`, i.e. by
//   renaming the store's data file to its final path (P21): the landed file
//   IS the seed.

/// The message every publish and replication path refuses with when no
/// Collaboration folder is set (collab v3 wave 2, P25). No silent fallback to
/// the working dir.
pub(crate) const COLLABORATION_ROOT_REQUIRED: &str =
    "set a Collaboration folder in File Manager → Folders first";

/// The configured Collaboration root — required for any collab receive or
/// publish (P25). Absent ⇒ `ApiError::Invalid` carrying
/// [`COLLABORATION_ROOT_REQUIRED`], logged at `warn!`.
pub(crate) fn require_collaboration_root(ctx: &ServiceContext) -> Result<PathBuf, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    match crate::db::scan_root_path_of_kind(&conn, "collaboration") {
        Ok(Some(path)) => Ok(PathBuf::from(path)),
        Ok(None) => {
            tracing::warn!(
                outcome = "no_collaboration_root",
                "collaboration root required"
            );
            Err(ApiError::Invalid(COLLABORATION_ROOT_REQUIRED.to_string()))
        }
        Err(e) => {
            tracing::error!(error = %e, "read collaboration root failed");
            Err(ApiError::Internal(format!("read collaboration root: {e}")))
        }
    }
}

/// A publisher's folder in a project (P10): the folder that publisher's rows
/// already use, else `<Collab>/<project slug>/<display name>`, sanitized and
/// made unique against every other publisher's folder in the project.
/// `fallback` names the folder when the display name is empty. Shared by
/// publish (my own folder) and landing (a peer's folder). A rename of the
/// account never moves files: the folder is found by account id.
pub(crate) fn publisher_folder(
    conn: &rusqlite::Connection,
    collab_root: &Path,
    project: &crate::db::collab::CollabProjectRow,
    account_id: &str,
    display: &str,
    fallback: &str,
) -> Result<PathBuf> {
    if !account_id.is_empty() {
        if let Some(dir) =
            crate::db::collab_frames::publisher_dir(conn, &project.project_id, account_id)?
        {
            return Ok(dir);
        }
    }
    let project_dir = collab_root.join(crate::sync::ingest::sanitize_slug(&project.slug));
    let taken: HashSet<PathBuf> = {
        let mut stmt = conn.prepare(
            "SELECT landed_path FROM project_frames_local
             WHERE project_id = ?1 AND publisher_account_id != ?2 AND landed_path IS NOT NULL",
        )?;
        let paths = stmt
            .query_map(rusqlite::params![project.project_id, account_id], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        paths
            .into_iter()
            .filter_map(|p| Path::new(&p).parent().map(Path::to_path_buf))
            .collect()
    };
    let base = crate::sync::ingest::sanitize_slug(if display.is_empty() {
        fallback
    } else {
        display
    });
    for n in 1..10_000 {
        let name = if n == 1 {
            base.clone()
        } else {
            format!("{base}_{n}")
        };
        let candidate = project_dir.join(name);
        if !taken.contains(&candidate) {
            return Ok(candidate);
        }
    }
    anyhow::bail!(
        "no free publisher folder name under {}",
        project_dir.display()
    )
}

/// Emitted once when the loss guard pauses a project's replication
/// ([`CollabReplicationPaused`], P14).
pub const COLLAB_REPLICATION_PAUSED_EVENT: &str = "collab-replication-paused";

/// Emitted once per replication fetch that landed or failed anything
/// ([`CollabFramesLanded`]) — a discrete outcome, never progress.
pub const COLLAB_FRAMES_LANDED_EVENT: &str = "collab-frames-landed";

/// The hub's cap on `add` (and `remove`) per `PUT …/holders/self` (P8).
const HOLDERS_CHUNK: usize = 10_000;

/// Frames per assignment run. Batches run back to back inside one pass until
/// the need set is empty or a batch lands nothing.
const FETCH_BATCH: usize = 200;

/// Which pass is running (spec §5.5, ruling R15).
///
/// - `Cadence` — the minutes timer: disk truth, the loss guard and the FULL
///   holder report, then the need set.
/// - `Kicked` — a version moved (the 15 s poll already applied the manifest):
///   manifest-driven work only — need set, fetch, land, holder delta. Never a
///   stat walk and never a full report.
/// - `Forced` — "Sync now": everything `Cadence` does, one project, the
///   auto-replicate toggle forced on (the role gate never is).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PassKind {
    Cadence,
    Kicked,
    Forced,
}

impl PassKind {
    fn as_str(self) -> &'static str {
        match self {
            PassKind::Cadence => "cadence",
            PassKind::Kicked => "kicked",
            PassKind::Forced => "forced",
        }
    }

    /// Disk truth, the loss guard and the full holder report run here.
    fn walks_disk(self) -> bool {
        !matches!(self, PassKind::Kicked)
    }
}

/// A project's local replication policy (spec §5.3). Every constraint is
/// optional; empty lists mean "all". Stored as JSON in
/// `collab_projects.policy_json`, never sent to the hub.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ReplicationPolicy {
    /// Canonical filters to replicate (the dictionary's `canonical`).
    #[serde(default)]
    pub filters: Vec<String>,
    /// Publisher account ids to replicate.
    #[serde(default)]
    pub publishers: Vec<String>,
    /// Only frames whose manifest `meta.fwhmArcsec` is at most this. A frame
    /// without the measurement does not match a set bound.
    #[serde(default)]
    pub max_fwhm_arcsec: Option<f64>,
    /// Only frames whose manifest `meta.starsDetected` is at least this. A
    /// frame without the measurement does not match a set bound.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub min_stars: Option<i64>,
    /// At most this many bytes of this project's replicas on disk, counting
    /// the ones already held.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub byte_budget: Option<i64>,
}

/// What a policy selects in a project, as of the local cache
/// (`preview_collab_policy` / `set_collab_policy`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct PolicyPreview {
    /// Published, accepted, not-declined peer frames the policy matches.
    pub frames: usize,
    #[ts(type = "number")]
    pub bytes: i64,
    /// Of those, already on disk.
    pub already_held: usize,
    /// What the next pass would fetch (byte budget applied).
    pub to_fetch: usize,
    #[ts(type = "number")]
    pub to_fetch_bytes: i64,
}

/// Payload of [`COLLAB_REPLICATION_PAUSED_EVENT`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabReplicationPaused {
    pub project_id: String,
    pub missing: usize,
    #[ts(type = "number")]
    pub missing_bytes: i64,
}

/// Payload of [`COLLAB_FRAMES_LANDED_EVENT`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabFramesLanded {
    pub project_id: String,
    pub landed: usize,
    pub failed: usize,
    pub awaiting_gc: usize,
}

/// The user's answer to a paused project (P14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum LossAction {
    /// Rescan the Collaboration root (the scanner repairs moved files), run
    /// disk truth again, unpause.
    Restore,
    /// Stop holding the missing frames (`locally_declined`), unpause.
    StopHolding,
}

/// What one disk-truth walk found (spec §5.5).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiskTruth {
    /// `(frame_uuid, content_version)` of every frame whose file is on disk
    /// with its recorded content — the full holder report's source (P8).
    pub present: Vec<(String, i32)>,
    /// Peer frames whose landed file vanished or was edited this pass.
    pub missing_replicas: Vec<String>,
    /// My own frames whose landed file vanished or was edited this pass.
    pub missing_own: Vec<String>,
    /// Bytes of `missing_replicas`.
    pub missing_bytes: i64,
    /// Files re-hashed because their `size:mtime` moved.
    pub rehashed: usize,
    /// Replicas that were on disk when the walk started (the loss guard's
    /// denominator).
    pub held_replicas: usize,
    /// Present frames parked because a byte-identical sibling went missing
    /// and took their shared store entry with it (P20/P24): their file is
    /// fine, but the store cannot serve it until GC drops the dead entry.
    pub parked: Vec<String>,
}

/// What one [`fetch_frames`] call did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FetchOutcome {
    pub landed: usize,
    pub failed: usize,
    pub awaiting_gc: usize,
}

impl std::ops::AddAssign for FetchOutcome {
    fn add_assign(&mut self, o: Self) {
        self.landed += o.landed;
        self.failed += o.failed;
        self.awaiting_gc += o.awaiting_gc;
    }
}

/// `"size:mtime_secs"` — the same spelling publish records.
fn size_mtime_from(meta: &std::fs::Metadata) -> String {
    let secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}:{secs}", meta.len())
}

/// A string field of a row's verbatim manifest JSON.
fn manifest_str(row: &LocalFrameRow, key: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&row.manifest_json)
        .ok()?
        .get(key)?
        .as_str()
        .map(str::to_string)
}

/// A numeric `meta` field of a row's verbatim manifest JSON.
fn manifest_meta_f64(row: &LocalFrameRow, key: &str) -> Option<f64> {
    serde_json::from_str::<serde_json::Value>(&row.manifest_json)
        .ok()?
        .get("meta")?
        .get(key)?
        .as_f64()
}

/// Does `row` pass the policy's filter / publisher / quality constraints?
fn policy_matches(row: &LocalFrameRow, policy: &ReplicationPolicy) -> bool {
    if !policy.filters.is_empty() && !policy.filters.contains(&row.filter_canonical) {
        return false;
    }
    if !policy.publishers.is_empty() && !policy.publishers.contains(&row.publisher_account_id) {
        return false;
    }
    if let Some(max) = policy.max_fwhm_arcsec {
        match manifest_meta_f64(row, "fwhmArcsec") {
            Some(fwhm) if fwhm <= max => {}
            _ => return false,
        }
    }
    if let Some(min) = policy.min_stars {
        match manifest_meta_f64(row, "starsDetected") {
            Some(stars) if stars >= min as f64 => {}
            _ => return false,
        }
    }
    true
}

/// Is `row` a peer frame this device may hold at all (before policy)?
fn replicable(row: &LocalFrameRow) -> bool {
    row.state == "published"
        && row.accepted
        && row.origin == FrameOrigin::Replica
        && !row.locally_declined
}

/// The need set (spec §5.3), pure: which rows to fetch now.
///
/// `published ∧ accepted ∧ replica ∧ ¬on_disk ∧ ¬locally_declined ∧
/// ¬awaiting_gc ∧ policy`, empty when the role forbids replication, the
/// toggle is off, or the loss guard paused the project. Ordered rarest first
/// (`holder_count`), then oldest (`createdAt`), then by uuid. A byte budget
/// counts the replicas already on disk and stops at the first frame that
/// would cross it.
pub(crate) fn frame_need(
    rows: &[LocalFrameRow],
    policy: &ReplicationPolicy,
    role_allows: bool,
    auto_on: bool,
    paused: bool,
) -> Vec<LocalFrameRow> {
    if !role_allows || !auto_on || paused {
        return Vec::new();
    }
    let mut need: Vec<(i64, String, &LocalFrameRow)> = rows
        .iter()
        .filter(|r| replicable(r) && !r.on_disk && !r.awaiting_gc && policy_matches(r, policy))
        .map(|r| {
            (
                r.holder_count,
                manifest_str(r, "createdAt").unwrap_or_default(),
                r,
            )
        })
        .collect();
    need.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.frame_uuid.cmp(&b.2.frame_uuid))
    });
    let mut out = Vec::with_capacity(need.len());
    match policy.byte_budget {
        None => out.extend(need.into_iter().map(|(_, _, r)| r.clone())),
        Some(budget) => {
            let mut held: i64 = rows
                .iter()
                .filter(|r| r.origin == FrameOrigin::Replica && r.on_disk)
                .map(|r| r.byte_size)
                .sum();
            for (_, _, r) in need {
                if held.saturating_add(r.byte_size) > budget {
                    break;
                }
                held += r.byte_size;
                out.push(r.clone());
            }
        }
    }
    out
}

/// The node bound on the context, if any (never binds one).
async fn bound_node(
    ctx: &ServiceContext,
) -> Option<Arc<crate::sharing::iroh::node::SharedIrohNode>> {
    ctx.iroh_node.lock().await.clone()
}

/// The health of a row's content in the collab store. `None` when no node or
/// no collab store is there to ask, or the hash does not parse (logged).
async fn frame_health(
    node: Option<&Arc<crate::sharing::iroh::node::SharedIrohNode>>,
    row: &LocalFrameRow,
) -> Option<crate::sharing::iroh::node::BlobHealth> {
    let node = node?;
    node.collab_store()?;
    let hash = match row.blake3.parse::<iroh_blobs::Hash>() {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(frame_uuid = %row.frame_uuid, error = %e, "frame blake3 does not parse");
            return None;
        }
    };
    node.collab_blob_health(hash).await.ok()
}

/// Full-file xxh3 on a blocking thread.
async fn xxh3_on_blocking(path: &Path) -> Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || crate::package::xxh3_full_file(&path))
        .await
        .context("xxh3 task join")?
}

/// Drop one frame's seed tags when a node with a collab store is bound.
/// Failures are logged inside [`unseed_project_frame`](crate::sharing::iroh::node::SharedIrohNode::unseed_project_frame).
async fn unseed_frame(
    node: Option<&Arc<crate::sharing::iroh::node::SharedIrohNode>>,
    project_id: &str,
    frame_uuid: &str,
) {
    if let Some(node) = node {
        if node.collab_store().is_some() {
            let _ = node.unseed_project_frame(project_id, frame_uuid).await;
        }
    }
}

/// Disk truth for one project (spec §5.5). For every row with a landed path:
///
/// - on disk: `stat` it; gone → MISSING. A `size:mtime` that moved →
///   re-hash; a different xxh3 is an edit and counts as MISSING, the same one
///   stores the new `size:mtime`. A missing frame is marked (`on_disk = 0`),
///   its seed tags are dropped, and a replica's `awaiting_gc` records whether
///   its store entry is already gone (`Missing`) or must be collected first
///   (anything else, P20 — never fetch over a live or dead entry).
/// - not on disk (lost earlier, or put back by a rescan): when the file at
///   the path has the recorded size and xxh3 and the store can take it
///   (entry `Missing` or `Readable`), it is seeded again and re-admitted.
///
/// A present frame whose byte-identical sibling went missing may have lost
/// its readable path with it (the store reads one path per entry, P24); such
/// a frame is parked (`awaiting_gc`) until GC drops the dead entry.
pub(crate) async fn disk_truth(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<DiskTruth, ApiError> {
    let rows = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab_frames::list_for_project(&conn, project_id)?
    };
    let node = bound_node(ctx).await;
    let mut truth = DiskTruth::default();
    let mut lost: Vec<&LocalFrameRow> = Vec::new();
    let mut present_rows: Vec<&LocalFrameRow> = Vec::new();

    for row in &rows {
        let Some(landed) = row.landed_path.as_deref() else {
            continue;
        };
        let path = Path::new(landed);
        if !row.on_disk {
            if row.locally_declined {
                continue;
            }
            if readmit(ctx, node.as_ref(), row, path).await? {
                tracing::info!(project_id, frame_uuid = %row.frame_uuid, path = %landed, "frame re-admitted from disk");
                truth
                    .present
                    .push((row.frame_uuid.clone(), row.content_version));
                present_rows.push(row);
            }
            continue;
        }
        if row.origin == FrameOrigin::Replica {
            truth.held_replicas += 1;
        }
        let meta = match tokio::fs::metadata(path).await {
            Ok(meta) if meta.is_file() => meta,
            Ok(_) => {
                tracing::warn!(project_id, frame_uuid = %row.frame_uuid, path = %landed, "landed frame path is not a file");
                lost.push(row);
                continue;
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    tracing::debug!(project_id, frame_uuid = %row.frame_uuid, path = %landed, "landed frame is gone");
                } else {
                    tracing::warn!(project_id, frame_uuid = %row.frame_uuid, path = %landed, error = %e, "stat of a landed frame failed; counted missing");
                }
                lost.push(row);
                continue;
            }
        };
        let sm = size_mtime_from(&meta);
        if row.size_mtime_seen.as_deref() == Some(sm.as_str()) {
            truth
                .present
                .push((row.frame_uuid.clone(), row.content_version));
            present_rows.push(row);
            continue;
        }
        truth.rehashed += 1;
        match xxh3_on_blocking(path).await {
            Ok(h) if h == row.xxh3 => {
                let db = db(ctx)?;
                crate::db::collab_frames::set_size_mtime_seen(
                    &db.conn(),
                    project_id,
                    &row.frame_uuid,
                    &sm,
                )?;
                truth
                    .present
                    .push((row.frame_uuid.clone(), row.content_version));
                present_rows.push(row);
            }
            Ok(h) => {
                tracing::warn!(project_id, frame_uuid = %row.frame_uuid, path = %landed, xxh3 = %h, "landed frame was edited; counted missing");
                lost.push(row);
            }
            Err(e) => {
                tracing::warn!(project_id, frame_uuid = %row.frame_uuid, path = %landed, error = %format!("{e:#}"), "re-hash of a landed frame failed; counted missing");
                lost.push(row);
            }
        }
    }

    for row in &lost {
        {
            let db = db(ctx)?;
            // Conservative first: `awaiting_gc` until the store says the
            // entry is gone.
            crate::db::collab_frames::set_missing(
                &db.conn(),
                project_id,
                &row.frame_uuid,
                row.origin == FrameOrigin::Replica,
            )?;
        }
        unseed_frame(node.as_ref(), project_id, &row.frame_uuid).await;
        match row.origin {
            FrameOrigin::Own => truth.missing_own.push(row.frame_uuid.clone()),
            FrameOrigin::Replica => {
                if frame_health(node.as_ref(), row).await
                    == Some(crate::sharing::iroh::node::BlobHealth::Missing)
                {
                    let db = db(ctx)?;
                    crate::db::collab_frames::set_missing(
                        &db.conn(),
                        project_id,
                        &row.frame_uuid,
                        false,
                    )?;
                }
                truth.missing_replicas.push(row.frame_uuid.clone());
                truth.missing_bytes = truth.missing_bytes.saturating_add(row.byte_size);
            }
        }
    }

    // P24: a present frame that shares its content with a lost one.
    let lost_hashes: HashSet<&str> = lost.iter().map(|r| r.blake3.as_str()).collect();
    for row in present_rows {
        if !lost_hashes.contains(row.blake3.as_str()) {
            continue;
        }
        if frame_health(node.as_ref(), row).await
            != Some(crate::sharing::iroh::node::BlobHealth::Dead)
        {
            continue;
        }
        tracing::warn!(project_id, frame_uuid = %row.frame_uuid, "identical frame lost its readable store path; parked until GC");
        {
            let db = db(ctx)?;
            crate::db::collab_frames::set_missing(&db.conn(), project_id, &row.frame_uuid, true)?;
        }
        unseed_frame(node.as_ref(), project_id, &row.frame_uuid).await;
        truth.present.retain(|(u, _)| u != &row.frame_uuid);
        truth.parked.push(row.frame_uuid.clone());
    }

    tracing::debug!(
        project_id,
        count = truth.present.len(),
        missing = truth.missing_replicas.len() + truth.missing_own.len(),
        rehashed = truth.rehashed,
        "disk truth"
    );
    Ok(truth)
}

/// Re-admit a frame whose file is back at its landed path (a rescan's
/// "moved" repair, a restored folder): right size, right xxh3, and a store
/// that can take it (entry `Missing` or `Readable` — never over a `Dead` or
/// `Partial` one, P20). Seeds it and marks it landed. `true` when re-admitted.
async fn readmit(
    ctx: &ServiceContext,
    node: Option<&Arc<crate::sharing::iroh::node::SharedIrohNode>>,
    row: &LocalFrameRow,
    path: &Path,
) -> Result<bool, ApiError> {
    use crate::sharing::iroh::node::BlobHealth;
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return Ok(false);
    };
    if !meta.is_file() || meta.len() as i64 != row.byte_size {
        return Ok(false);
    }
    let Some(node) = node else {
        return Ok(false);
    };
    if !matches!(
        frame_health(Some(node), row).await,
        Some(BlobHealth::Missing | BlobHealth::Readable)
    ) {
        return Ok(false);
    }
    match xxh3_on_blocking(path).await {
        Ok(h) if h == row.xxh3 => {}
        Ok(_) => return Ok(false),
        Err(e) => {
            tracing::debug!(frame_uuid = %row.frame_uuid, error = %format!("{e:#}"), "re-admission hash failed");
            return Ok(false);
        }
    }
    if node
        .seed_project_frame(&row.project_id, &row.frame_uuid, row.content_version, path)
        .await
        .is_err()
    {
        // Logged inside; a Dead entry is parked by the next pass.
        return Ok(false);
    }
    let sm = size_mtime_from(&meta);
    let db = db(ctx)?;
    crate::db::collab_frames::set_landed(
        &db.conn(),
        &row.project_id,
        &row.frame_uuid,
        &path.to_string_lossy(),
        &sm,
    )?;
    Ok(true)
}

/// Clear `awaiting_gc` on every parked replica whose store entry is gone
/// (`Missing`) or only partial — the need set may fetch it again (P20). Runs
/// in every pass kind: it is one store lookup per parked row, no disk walk.
async fn recheck_awaiting_gc(ctx: &ServiceContext, project_id: &str) -> Result<usize, ApiError> {
    use crate::sharing::iroh::node::BlobHealth;
    let node = bound_node(ctx).await;
    if node.as_ref().and_then(|n| n.collab_store()).is_none() {
        return Ok(0);
    }
    let parked: Vec<LocalFrameRow> = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab_frames::list_for_project(&conn, project_id)?
            .into_iter()
            .filter(|r| r.awaiting_gc && !r.on_disk)
            .collect()
    };
    let mut cleared = 0;
    for row in &parked {
        if matches!(
            frame_health(node.as_ref(), row).await,
            Some(BlobHealth::Missing | BlobHealth::Partial)
        ) {
            let db = db(ctx)?;
            crate::db::collab_frames::set_missing(&db.conn(), project_id, &row.frame_uuid, false)?;
            cleared += 1;
        }
    }
    if cleared > 0 {
        tracing::info!(
            project_id,
            count = cleared,
            "collected frames may be fetched again"
        );
    }
    Ok(cleared)
}

/// The loss guard (P14): when this pass's missing replicas exceed
/// `collab.loss_guard_fraction` of the held ones, or `collab.loss_guard_bytes`
/// — and at least two are missing, so one deletion never trips it — pause the
/// project's replication and emit [`COLLAB_REPLICATION_PAUSED_EVENT`] once.
/// Returns whether it tripped. The caller skips it when the project is
/// already paused.
pub(crate) fn loss_guard(
    ctx: &ServiceContext,
    project_id: &str,
    truth: &DiskTruth,
    emitter: Option<&dyn ProgressEmitter>,
) -> Result<bool, ApiError> {
    let missing = truth.missing_replicas.len();
    if missing < 2 {
        return Ok(false);
    }
    let db = db(ctx)?;
    let conn = db.conn();
    let fraction = ctx
        .settings
        .get_collab_loss_guard_fraction(&conn)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %format!("{e:#}"), "loss guard fraction unreadable; default used");
            crate::settings::defaults::COLLAB_LOSS_GUARD_FRACTION
                .parse()
                .unwrap_or(0.10)
        });
    let bytes = ctx
        .settings
        .get_collab_loss_guard_bytes(&conn)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %format!("{e:#}"), "loss guard bytes unreadable; default used");
            crate::settings::defaults::COLLAB_LOSS_GUARD_BYTES
                .parse()
                .unwrap_or(i64::MAX)
        });
    let held = truth.held_replicas.max(missing);
    let tripped = (missing as f64 / held as f64) > fraction || truth.missing_bytes > bytes;
    if !tripped {
        return Ok(false);
    }
    crate::db::collab::set_replication_paused(&conn, project_id, true)?;
    drop(conn);
    tracing::warn!(
        project_id,
        count = missing,
        bytes = truth.missing_bytes,
        "replication paused by the loss guard"
    );
    if let Some(em) = emitter {
        crate::events::emit_event(
            em,
            COLLAB_REPLICATION_PAUSED_EVENT,
            &CollabReplicationPaused {
                project_id: project_id.to_string(),
                missing,
                missing_bytes: truth.missing_bytes,
            },
        );
    }
    Ok(true)
}

/// The FULL holder report (P8): `PUT …/holders/self` with exactly the frames
/// on disk. Above [`HOLDERS_CHUNK`] frames the first chunk goes `full` and
/// the rest as `add`. An empty set is still sent — "I hold nothing here" is
/// what clears a phantom holder (audit F5).
///
/// Signed out ⇒ `Ok(0)`. A 403 folds into `Ok(0)` at `debug!` (a membership
/// the hub will not take holds from); anything else is returned for the
/// caller to log and step over.
pub(crate) async fn report_holders(
    ctx: &ServiceContext,
    project_id: &str,
    present: &[(String, i32)],
) -> Result<usize, ApiError> {
    use crate::collab::hub_client::HolderRefWire;
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        return Ok(0);
    };
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    let refs: Vec<HolderRefWire> = present
        .iter()
        .map(|(u, v)| HolderRefWire {
            frame_uuid: u.clone(),
            content_version: *v,
        })
        .collect();
    let chunks: Vec<&[HolderRefWire]> = if refs.is_empty() {
        vec![&refs[..]]
    } else {
        refs.chunks(HOLDERS_CHUNK).collect()
    };
    for (i, chunk) in chunks.into_iter().enumerate() {
        match client
            .put_holders(&token, project_id, i == 0, chunk, &[])
            .await
        {
            Ok(()) => {}
            Err(crate::account::AccountClientError::Forbidden) => {
                tracing::debug!(project_id, count = refs.len(), "holder report forbidden");
                return Ok(0);
            }
            Err(e) => {
                tracing::warn!(project_id, count = refs.len(), error = %e, "holder report failed");
                return Err(client_err(e));
            }
        }
    }
    tracing::info!(project_id, count = refs.len(), "holders reported");
    Ok(refs.len())
}

/// One replication fetch per project at a time, per catalog (ruling on the
/// audit's `PackagePullClaim`): a "Sync now" that lands while the worker is
/// fetching the same project returns at once — the running fetch IS the
/// requested work. Released on every exit, a panic included.
static IN_FLIGHT_FRAME_PULLS: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> =
    std::sync::OnceLock::new();

struct FramePullClaim(String);

impl FramePullClaim {
    fn acquire(key: &str) -> Option<Self> {
        let set = IN_FLIGHT_FRAME_PULLS.get_or_init(Default::default);
        match set.lock() {
            Ok(mut set) => set
                .insert(key.to_string())
                .then(|| FramePullClaim(key.to_string())),
            // Permissive on poison: refusing every fetch after one unrelated
            // panic would be the worse failure.
            Err(_) => Some(FramePullClaim(key.to_string())),
        }
    }
}

impl Drop for FramePullClaim {
    fn drop(&mut self) {
        if let Some(set) = IN_FLIGHT_FRAME_PULLS.get() {
            if let Ok(mut set) = set.lock() {
                set.remove(&self.0);
            }
        }
    }
}

/// Record a frame-level failure on its row (logged by the caller).
fn record_frame_error(ctx: &ServiceContext, project_id: &str, frame_uuid: &str, error: &str) {
    match db(ctx) {
        Ok(db) => {
            if let Err(e) =
                crate::db::collab_frames::set_error(&db.conn(), project_id, frame_uuid, Some(error))
            {
                tracing::warn!(project_id, frame_uuid, error = %format!("{e:#}"), "record frame error failed");
            }
        }
        Err(e) => tracing::warn!(project_id, frame_uuid, error = %e, "record frame error failed"),
    }
}

/// Everything one fetch needs, resolved once per [`fetch_frames`] call.
struct FetchEnv<'a> {
    ctx: &'a ServiceContext,
    node: Arc<crate::sharing::iroh::node::SharedIrohNode>,
    store: iroh_blobs::api::Store,
    client: CollabClient,
    token: String,
    relay_urls: Vec<String>,
    own: NodeId,
    project: crate::db::collab::CollabProjectRow,
    collab_root: PathBuf,
    started_at: String,
}

/// Fetch and land `need` (P11, P21, P24): batches of [`FETCH_BATCH`] frames
/// back to back until the set is empty or a batch lands nothing. Each batch
/// is ONE assignment run on the collab ALPN, every frame with its own fresh
/// holders (minus this device) dialled relay-only; one frame failing never
/// aborts its siblings. One `ReceiveGate` permit covers the whole fetch
/// (ruling R2).
///
/// Before a frame is fetched its store entry is checked (P20): a `Dead`
/// entry is parked for GC (a fetch over it would panic iroh-blobs 0.103); a
/// `Readable` one whose content another frame of the project already landed
/// is linked from that file instead (P24). After each batch the landed frames
/// go to the hub as one holder `add` (P8).
pub(crate) async fn fetch_frames(
    ctx: &ServiceContext,
    sync: &crate::sync::SyncRuntime,
    project_id: &str,
    need: Vec<LocalFrameRow>,
    emitter: Option<&dyn ProgressEmitter>,
) -> Result<FetchOutcome, ApiError> {
    let mut outcome = FetchOutcome::default();
    if need.is_empty() {
        return Ok(outcome);
    }
    let claim_key = format!("{}|{project_id}", db(ctx)?.path().display());
    let Some(_claim) = FramePullClaim::acquire(&claim_key) else {
        tracing::info!(
            project_id,
            "replication skipped: a fetch of this project is already running"
        );
        return Ok(outcome);
    };
    let collab_root = require_collaboration_root(ctx)?;
    let project = {
        let db = db(ctx)?;
        let conn = db.conn();
        live_project(&conn, project_id)?
    };
    let Some(node) = bound_node(ctx).await else {
        let e = ApiError::Internal("sync transport not started; cannot replicate".into());
        tracing::warn!(project_id, error = %e, "replication fetch refused");
        return Err(e);
    };
    let Some(store) = node.collab_store() else {
        let e = ApiError::Internal("the collaboration store is not mounted".into());
        tracing::warn!(project_id, error = %e, "replication fetch refused");
        return Err(e);
    };
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        tracing::debug!(project_id, "replication fetch skipped: signed out");
        return Ok(outcome);
    };
    let client = CollabClient::new(&hub_url).map_err(client_err)?;
    // The node's own relay set (already resolved and applied at bind / on
    // refresh) completes each holder's hint; no hub round trip here.
    let relay_urls = node.relay_urls();
    let own = node.node_id();

    // Ruling R2: one permit for the project's whole fetch. No receiver
    // started ⇒ no competing receives to be fair to.
    let _receive_permit = match sync.inbound_control().await {
        Some(control) => Some(control.receive_gate.acquire().await),
        None => None,
    };

    let env = FetchEnv {
        ctx,
        node,
        store,
        client,
        token,
        relay_urls,
        own,
        project,
        collab_root,
        started_at: crate::sync::now_iso(),
    };
    let total = need.len();
    tracing::info!(project_id, count = total, "replication fetch started");
    let mut queue: std::collections::VecDeque<LocalFrameRow> = need.into();
    let mut hinted: HashSet<NodeId> = HashSet::new();
    while !queue.is_empty() {
        let take = queue.len().min(FETCH_BATCH);
        let batch: Vec<LocalFrameRow> = queue.drain(..take).collect();
        let b = fetch_batch(&env, batch, &mut hinted).await?;
        outcome += b;
        if b.landed == 0 {
            if !queue.is_empty() {
                tracing::info!(
                    project_id,
                    count = queue.len(),
                    "replication stops early: a batch landed nothing"
                );
            }
            break;
        }
    }
    tracing::info!(
        project_id,
        count = total,
        landed = outcome.landed,
        failed = outcome.failed,
        awaiting_gc = outcome.awaiting_gc,
        "replication fetch finished"
    );
    if let Some(em) = emitter {
        if outcome.landed + outcome.failed + outcome.awaiting_gc > 0 {
            crate::events::emit_event(
                em,
                COLLAB_FRAMES_LANDED_EVENT,
                &CollabFramesLanded {
                    project_id: project_id.to_string(),
                    landed: outcome.landed,
                    failed: outcome.failed,
                    awaiting_gc: outcome.awaiting_gc,
                },
            );
        }
    }
    Ok(outcome)
}

/// The project row, refusing an unknown or lost project (R14: a lost project
/// is never acted on).
fn live_project(
    conn: &rusqlite::Connection,
    project_id: &str,
) -> Result<crate::db::collab::CollabProjectRow, ApiError> {
    let row = crate::db::collab::get_project(conn, project_id)?;
    match row {
        Some(row) if crate::db::collab::lost_at(conn, project_id)?.is_none() => Ok(row),
        _ => Err(ApiError::Invalid(format!("unknown project {project_id}"))),
    }
}

/// A landed, on-disk frame of the same project with this content (P24).
fn identical_landed(
    ctx: &ServiceContext,
    row: &LocalFrameRow,
) -> Result<Option<PathBuf>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    Ok(
        crate::db::collab_frames::find_by_project_and_xxh3(&conn, &row.project_id, &row.xxh3)?
            .into_iter()
            .filter(|r| r.frame_uuid != row.frame_uuid && r.on_disk && r.blake3 == row.blake3)
            .filter_map(|r| r.landed_path.map(PathBuf::from))
            .find(|p| p.is_file()),
    )
}

/// One batch of [`fetch_frames`].
async fn fetch_batch(
    env: &FetchEnv<'_>,
    batch: Vec<LocalFrameRow>,
    hinted: &mut HashSet<NodeId>,
) -> Result<FetchOutcome, ApiError> {
    use crate::collab::hub_client::HolderRefWire;
    use crate::sharing::iroh::blobs::{self, FrameFetch};
    use crate::sharing::iroh::node::{project_frame_tag, BlobHealth};

    let pid = env.project.project_id.as_str();
    let mut outcome = FetchOutcome::default();
    let mut landed: Vec<HolderRefWire> = Vec::new();
    let mut fetches: Vec<FrameFetch> = Vec::new();
    let mut fetch_rows: std::collections::HashMap<String, (LocalFrameRow, iroh_blobs::Hash)> =
        Default::default();
    let mut rep_of_hash: std::collections::HashMap<iroh_blobs::Hash, String> = Default::default();
    let mut followers: std::collections::HashMap<String, Vec<LocalFrameRow>> = Default::default();
    let mut local: Vec<(LocalFrameRow, iroh_blobs::Hash)> = Vec::new();

    for row in batch {
        let uuid = row.frame_uuid.clone();
        let hash = match row.blake3.parse::<iroh_blobs::Hash>() {
            Ok(h) => h,
            Err(e) => {
                let msg = format!("frame blake3 does not parse: {e}");
                tracing::warn!(project_id = pid, frame_uuid = %uuid, error = %msg, "frame skipped");
                record_frame_error(env.ctx, pid, &uuid, &msg);
                outcome.failed += 1;
                continue;
            }
        };
        if let Some(rep) = rep_of_hash.get(&hash) {
            followers.entry(rep.clone()).or_default().push(row);
            continue;
        }
        let health = match env.node.collab_blob_health(hash).await {
            Ok(h) => h,
            Err(e) => {
                record_frame_error(env.ctx, pid, &uuid, &format!("{e:#}"));
                outcome.failed += 1;
                continue;
            }
        };
        match health {
            BlobHealth::Dead => {
                // P20: never fetch over a dead entry. Park until GC.
                tracing::warn!(project_id = pid, frame_uuid = %uuid, "frame content is a dead store entry; waiting for GC");
                unseed_frame(Some(&env.node), pid, &uuid).await;
                let db = db(env.ctx)?;
                crate::db::collab_frames::set_missing(&db.conn(), pid, &uuid, true)?;
                outcome.awaiting_gc += 1;
                continue;
            }
            BlobHealth::Readable => {
                if let Some(src) = identical_landed(env.ctx, &row)? {
                    match link_identical(env, &row, &src).await {
                        Some(r) => {
                            landed.push(r);
                            outcome.landed += 1;
                        }
                        None => outcome.failed += 1,
                    }
                    continue;
                }
                // Complete in the store already (a fetch that died before
                // its landing): land it straight from the store.
                rep_of_hash.insert(hash, uuid.clone());
                local.push((row, hash));
                continue;
            }
            BlobHealth::Missing | BlobHealth::Partial => {}
        }
        let holders = match env.client.frame_holders(&env.token, pid, &uuid).await {
            Ok(h) => h,
            Err(crate::account::AccountClientError::CollabApiOutdated) => {
                return Err(client_err(
                    crate::account::AccountClientError::CollabApiOutdated,
                ));
            }
            Err(e) => {
                let msg = format!("holder lookup failed: {e}");
                tracing::warn!(project_id = pid, frame_uuid = %uuid, error = %e, "frame holder lookup failed");
                record_frame_error(env.ctx, pid, &uuid, &msg);
                outcome.failed += 1;
                continue;
            }
        };
        let providers: Vec<(NodeId, Option<String>)> = holders
            .iter()
            .filter_map(|h| {
                pairing::node_id_from_pubkey_b64(&h.pubkey)
                    .ok()
                    .map(|n| (n, h.relay_url.clone()))
            })
            .filter(|(n, _)| *n != env.own)
            .collect();
        if providers.is_empty() {
            tracing::debug!(project_id = pid, frame_uuid = %uuid, "no fresh holder");
            continue;
        }
        // Dial hints, once per provider per fetch. Cross-account: relay only
        // (S1). A hint that cannot be built costs that provider its dial, not
        // the frame — the others may still serve.
        for (holder, relay) in &providers {
            if !hinted.insert(*holder) {
                continue;
            }
            let reported = relay
                .as_ref()
                .map(|url| crate::account::EndpointAddrReport {
                    home_relay_url: Some(url.clone()),
                    direct_addrs: Vec::new(),
                    reported_at: None,
                });
            match pairing::peer_dial_addr(*holder, reported.as_ref(), &env.relay_urls, true) {
                Ok(addr) => env.node.add_peer(addr),
                Err(e) => tracing::warn!(
                    error = %format!("{e:#}"),
                    holder = %node_id_hex(holder),
                    "replication: dial hint build failed for one provider"
                ),
            }
        }
        let endpoints: Vec<iroh::EndpointId> = providers
            .iter()
            .filter_map(|(n, _)| iroh::EndpointId::from_bytes(n).ok())
            .collect();
        fetches.push(FrameFetch {
            key: uuid.clone(),
            hash,
            size: row.byte_size.max(0) as u64,
            providers: endpoints,
            in_flight_tag: blobs::in_flight_tag(&project_frame_tag(
                pid,
                &uuid,
                row.content_version,
            )),
        });
        rep_of_hash.insert(hash, uuid.clone());
        fetch_rows.insert(uuid, (row, hash));
    }

    // Frames already complete in the store: protect, then land.
    let mut to_land: Vec<(LocalFrameRow, iroh_blobs::Hash)> = Vec::new();
    for (row, hash) in local {
        let tag = blobs::in_flight_tag(&project_frame_tag(
            pid,
            &row.frame_uuid,
            row.content_version,
        ));
        if let Err(e) = env
            .store
            .tags()
            .set(&tag, iroh_blobs::HashAndFormat::raw(hash))
            .await
        {
            let msg = format!("set in-flight tag {tag}: {e}");
            tracing::warn!(project_id = pid, frame_uuid = %row.frame_uuid, error = %msg, "frame skipped");
            record_frame_error(env.ctx, pid, &row.frame_uuid, &msg);
            outcome.failed += 1;
            continue;
        }
        to_land.push((row, hash));
    }

    if !fetches.is_empty() {
        let telemetry: ProviderTelemetrySink = {
            let pid = pid.to_string();
            Arc::new(move |ev| match ev {
                ProviderEvent::Trying(id) => {
                    tracing::debug!(project_id = %pid, holder = %node_id_hex(&id), "replication: provider tried")
                }
                ProviderEvent::Failed(id) => {
                    tracing::debug!(project_id = %pid, holder = %node_id_hex(&id), "replication: provider failed; switching")
                }
            })
        };
        let tags: Vec<String> = fetches.iter().map(|f| f.in_flight_tag.clone()).collect();
        let results = match blobs::fetch_blobs_assigned(
            &env.store,
            &env.node.endpoint(),
            fetches,
            telemetry,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(project_id = pid, error = %format!("{e:#}"), "replication batch fetch failed");
                for tag in &tags {
                    drop_tag(&env.store, tag).await;
                }
                for (uuid, _) in fetch_rows.drain() {
                    record_frame_error(env.ctx, pid, &uuid, &format!("{e:#}"));
                    outcome.failed += 1 + followers.remove(&uuid).map_or(0, |f| f.len());
                }
                return Ok(outcome);
            }
        };
        for (uuid, r) in results {
            let Some((row, hash)) = fetch_rows.remove(&uuid) else {
                continue;
            };
            match r {
                Ok(()) => to_land.push((row, hash)),
                Err(e) => {
                    let msg = format!("{e:#}");
                    tracing::warn!(project_id = pid, frame_uuid = %uuid, error = %msg, "frame fetch failed");
                    record_frame_error(env.ctx, pid, &uuid, &msg);
                    drop_tag(
                        &env.store,
                        &blobs::in_flight_tag(&project_frame_tag(pid, &uuid, row.content_version)),
                    )
                    .await;
                    outcome.failed += 1;
                    for f in followers.remove(&uuid).unwrap_or_default() {
                        record_frame_error(env.ctx, pid, &f.frame_uuid, &msg);
                        outcome.failed += 1;
                    }
                }
            }
        }
    }

    for (row, hash) in to_land {
        let uuid = row.frame_uuid.clone();
        match land_frame(env, &row, hash).await {
            Landed::Yes(dest) => {
                landed.push(HolderRefWire {
                    frame_uuid: uuid.clone(),
                    content_version: row.content_version,
                });
                outcome.landed += 1;
                for f in followers.remove(&uuid).unwrap_or_default() {
                    match link_identical(env, &f, &dest).await {
                        Some(r) => {
                            landed.push(r);
                            outcome.landed += 1;
                        }
                        None => outcome.failed += 1,
                    }
                }
            }
            Landed::AwaitingGc => {
                outcome.awaiting_gc += 1;
                outcome.failed += followers.remove(&uuid).map_or(0, |f| f.len());
            }
            Landed::Failed => {
                outcome.failed += 1 + followers.remove(&uuid).map_or(0, |f| f.len());
            }
        }
    }

    if !landed.is_empty() {
        // Folded after logging: the cadence pass's full report repairs a
        // missed delta (P8).
        if let Err(e) = env
            .client
            .put_holders(&env.token, pid, false, &landed, &[])
            .await
        {
            tracing::warn!(project_id = pid, count = landed.len(), error = %e, "holder delta after landing failed");
        }
    }
    Ok(outcome)
}

/// Delete one tag, logging a failure (the store's open sweep reclaims a
/// stale in-flight tag).
async fn drop_tag(store: &iroh_blobs::api::Store, tag: &str) {
    if let Err(e) = store.tags().delete(tag).await {
        tracing::warn!(tag, error = %e, "delete tag failed");
    }
}

/// What landing one frame did.
enum Landed {
    Yes(PathBuf),
    /// The store's data went away under us (export found no source): the
    /// tags are dropped and the frame waits for GC (P20). Not a failure.
    AwaitingGc,
    /// Logged and recorded on the row.
    Failed,
}

/// Land one fetched frame (P21): `export_child` straight to its final path —
/// a rename of the store's data file, so the landed file IS the seed — then
/// the permanent seed tag, then the in-flight tag goes, then one DB
/// transaction (row + `sync_history`).
///
/// The path: a row that already has one (a new content version, or a frame
/// fetched again after it went missing) lands OVER it under the same name —
/// the old version's tags are dropped first so its entry is left to GC, and
/// `export_child` removes the stale file. Otherwise
/// `unique_path(<Collab>/<project>/<publisher>/<fileName>)`.
async fn land_frame(env: &FetchEnv<'_>, row: &LocalFrameRow, hash: iroh_blobs::Hash) -> Landed {
    use crate::sharing::iroh::blobs;
    use crate::sharing::iroh::node::project_frame_tag;

    let pid = env.project.project_id.as_str();
    let uuid = row.frame_uuid.as_str();
    let in_flight = blobs::in_flight_tag(&project_frame_tag(pid, uuid, row.content_version));
    let fail = |msg: String| {
        tracing::error!(project_id = pid, frame_uuid = uuid, error = %msg, "frame landing failed");
        record_frame_error(env.ctx, pid, uuid, &msg);
    };

    let dest = match row.landed_path.as_deref() {
        Some(existing) => {
            unseed_frame(Some(&env.node), pid, uuid).await;
            PathBuf::from(existing)
        }
        None => {
            if let Err(e) = crate::package::validate_rel_path(&row.file_name) {
                fail(format!("unsafe file name {:?}: {e:#}", row.file_name));
                drop_tag(&env.store, &in_flight).await;
                return Landed::Failed;
            }
            let dir = match db(env.ctx).map_err(anyhow::Error::from).and_then(|db| {
                publisher_folder(
                    &db.conn(),
                    &env.collab_root,
                    &env.project,
                    &row.publisher_account_id,
                    &row.publisher_display,
                    "publisher",
                )
            }) {
                Ok(dir) => dir,
                Err(e) => {
                    fail(format!("publisher folder: {e:#}"));
                    drop_tag(&env.store, &in_flight).await;
                    return Landed::Failed;
                }
            };
            crate::sync::ingest::unique_path(
                &dir.join(crate::sync::ingest::native_rel_path(&row.file_name)),
            )
        }
    };
    if let Some(parent) = dest.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            fail(format!("create {}: {e}", parent.display()));
            drop_tag(&env.store, &in_flight).await;
            return Landed::Failed;
        }
    }
    match blobs::export_child(&env.store, hash, &dest).await {
        Err(e) => {
            fail(format!("export to {}: {e:#}", dest.display()));
            drop_tag(&env.store, &in_flight).await;
            return Landed::Failed;
        }
        Ok(Err(e)) if blobs::export_source_vanished(&e) => {
            tracing::warn!(project_id = pid, frame_uuid = uuid, path = %dest.display(), error = %e, "frame data vanished before landing; waiting for GC");
            drop_tag(&env.store, &in_flight).await;
            unseed_frame(Some(&env.node), pid, uuid).await;
            if let Ok(db) = db(env.ctx) {
                if let Err(e) = crate::db::collab_frames::set_missing(&db.conn(), pid, uuid, true) {
                    tracing::warn!(project_id = pid, frame_uuid = uuid, error = %format!("{e:#}"), "mark frame awaiting GC failed");
                }
            }
            return Landed::AwaitingGc;
        }
        Ok(Err(e)) => {
            fail(format!("export to {}: {e}", dest.display()));
            drop_tag(&env.store, &in_flight).await;
            return Landed::Failed;
        }
        Ok(Ok(())) => {}
    }
    let tag = project_frame_tag(pid, uuid, row.content_version);
    if let Err(e) = env
        .store
        .tags()
        .set(&tag, iroh_blobs::HashAndFormat::raw(hash))
        .await
    {
        fail(format!("seed tag {tag}: {e}"));
        remove_landed(&dest);
        drop_tag(&env.store, &in_flight).await;
        return Landed::Failed;
    }
    drop_tag(&env.store, &in_flight).await;
    match record_landing(env, row, &dest) {
        Ok(()) => {
            tracing::info!(project_id = pid, frame_uuid = uuid, path = %dest.display(), "frame landed");
            Landed::Yes(dest)
        }
        Err(e) => {
            fail(format!("record landing: {e:#}"));
            remove_landed(&dest);
            unseed_frame(Some(&env.node), pid, uuid).await;
            Landed::Failed
        }
    }
}

/// Remove a landed file after a failed step, logging a failure.
fn remove_landed(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        tracing::warn!(path = %path.display(), error = %e, "remove orphaned landed frame failed");
    }
}

/// One transaction: the row is landed and a `sync_history` row records the
/// receive, the way the package ingest wrote it.
fn record_landing(env: &FetchEnv<'_>, row: &LocalFrameRow, dest: &Path) -> Result<()> {
    let meta = std::fs::metadata(dest).with_context(|| format!("stat {}", dest.display()))?;
    let sm = size_mtime_from(&meta);
    let db = db(env.ctx).map_err(|e| anyhow!("{e}"))?;
    let conn = db.conn();
    let tx = conn.unchecked_transaction().context("begin landing tx")?;
    crate::db::collab_frames::set_landed(
        &tx,
        &row.project_id,
        &row.frame_uuid,
        &dest.to_string_lossy(),
        &sm,
    )
    .context("mark frame landed")?;
    crate::sync::store::insert_history_row(
        &tx,
        &crate::sync::HistoryRow {
            frame_uuid: row.frame_uuid.clone(),
            filename: row.file_name.clone(),
            object: None,
            // A swarm fetch has no single serving device.
            peer_device: "swarm".to_string(),
            direction: crate::sync::Direction::Received,
            bytes: row.byte_size.max(0) as u64,
            started_at: env.started_at.clone(),
            finished_at: Some(crate::sync::now_iso()),
            outcome: "ingested".to_string(),
            project: Some(row.project_id.clone()),
            package_id: None,
            batch_name: None,
        },
    )
    .context("insert sync_history row")?;
    tx.commit().context("commit landing tx")?;
    Ok(())
}

/// P24: land a frame whose content another frame of the project already has
/// on disk — link (or copy) that file, seed it by reference, record it. No
/// fetch. Returns the holder entry on success; failures are logged and
/// recorded on the row.
async fn link_identical(
    env: &FetchEnv<'_>,
    row: &LocalFrameRow,
    src: &Path,
) -> Option<crate::collab::hub_client::HolderRefWire> {
    let pid = env.project.project_id.as_str();
    let uuid = row.frame_uuid.as_str();
    let fail = |msg: String| {
        tracing::warn!(project_id = pid, frame_uuid = uuid, error = %msg, "identical frame landing failed");
        record_frame_error(env.ctx, pid, uuid, &msg);
    };
    tracing::warn!(project_id = pid, frame_uuid = uuid, path = %src.display(), "identical frame content in project");
    let dest = match row.landed_path.as_deref() {
        Some(p) => {
            unseed_frame(Some(&env.node), pid, uuid).await;
            let p = PathBuf::from(p);
            if p != src {
                if let Err(e) = std::fs::remove_file(&p) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        fail(format!("remove stale {}: {e}", p.display()));
                        return None;
                    }
                }
            }
            p
        }
        None => {
            if let Err(e) = crate::package::validate_rel_path(&row.file_name) {
                fail(format!("unsafe file name {:?}: {e:#}", row.file_name));
                return None;
            }
            let dir = match db(env.ctx).map_err(anyhow::Error::from).and_then(|db| {
                publisher_folder(
                    &db.conn(),
                    &env.collab_root,
                    &env.project,
                    &row.publisher_account_id,
                    &row.publisher_display,
                    "publisher",
                )
            }) {
                Ok(dir) => dir,
                Err(e) => {
                    fail(format!("publisher folder: {e:#}"));
                    return None;
                }
            };
            crate::sync::ingest::unique_path(
                &dir.join(crate::sync::ingest::native_rel_path(&row.file_name)),
            )
        }
    };
    if let Some(parent) = dest.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            fail(format!("create {}: {e}", parent.display()));
            return None;
        }
    }
    if let Err(e) = crate::sync::ingest::link_or_copy(src, &dest, false) {
        fail(format!(
            "link {} -> {}: {e:#}",
            src.display(),
            dest.display()
        ));
        return None;
    }
    if let Err(e) = env
        .node
        .seed_project_frame(pid, uuid, row.content_version, &dest)
        .await
    {
        fail(format!("seed {}: {e:#}", dest.display()));
        remove_landed(&dest);
        return None;
    }
    if let Err(e) = record_landing(env, row, &dest) {
        fail(format!("record landing: {e:#}"));
        remove_landed(&dest);
        unseed_frame(Some(&env.node), pid, uuid).await;
        return None;
    }
    tracing::info!(project_id = pid, frame_uuid = uuid, path = %dest.display(), "frame landed");
    Some(crate::collab::hub_client::HolderRefWire {
        frame_uuid: row.frame_uuid.clone(),
        content_version: row.content_version,
    })
}

// ── Replication policy + loss commands ───────────────────────────────────────

/// The stored policy of a live project. An unreadable document is logged and
/// read as the default (replicate everything) — the same as never set.
fn read_policy(project: &crate::db::collab::CollabProjectRow) -> ReplicationPolicy {
    serde_json::from_str(&project.policy_json).unwrap_or_else(|e| {
        tracing::warn!(project_id = %project.project_id, error = %e, "replication policy unreadable; replicating everything");
        ReplicationPolicy::default()
    })
}

/// What `policy` selects in the project's cached frames.
fn policy_preview(rows: &[LocalFrameRow], policy: &ReplicationPolicy) -> PolicyPreview {
    let matching: Vec<&LocalFrameRow> = rows
        .iter()
        .filter(|r| replicable(r) && policy_matches(r, policy))
        .collect();
    let to_fetch = frame_need(rows, policy, true, true, false);
    PolicyPreview {
        frames: matching.len(),
        bytes: matching.iter().map(|r| r.byte_size).sum(),
        already_held: matching.iter().filter(|r| r.on_disk).count(),
        to_fetch: to_fetch.len(),
        to_fetch_bytes: to_fetch.iter().map(|r| r.byte_size).sum(),
    }
}

fn validate_policy(policy: &ReplicationPolicy) -> Result<(), ApiError> {
    if let Some(f) = policy.max_fwhm_arcsec {
        if !f.is_finite() || f <= 0.0 {
            return Err(ApiError::Invalid(
                "the FWHM limit must be a positive number of arcseconds".into(),
            ));
        }
    }
    if policy.min_stars.is_some_and(|s| s < 0) {
        return Err(ApiError::Invalid(
            "the star count cannot be negative".into(),
        ));
    }
    if policy.byte_budget.is_some_and(|b| b < 0) {
        return Err(ApiError::Invalid(
            "the byte budget cannot be negative".into(),
        ));
    }
    Ok(())
}

/// The project's replication policy (local only).
pub async fn get_collab_policy(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<ReplicationPolicy, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let project = live_project(&conn, project_id)?;
    Ok(read_policy(&project))
}

/// What `policy` would select, without storing it.
pub async fn preview_collab_policy(
    ctx: &ServiceContext,
    project_id: &str,
    policy: ReplicationPolicy,
) -> Result<PolicyPreview, ApiError> {
    validate_policy(&policy)?;
    let db = db(ctx)?;
    let conn = db.conn();
    live_project(&conn, project_id)?;
    let rows = crate::db::collab_frames::list_for_project(&conn, project_id)?;
    Ok(policy_preview(&rows, &policy))
}

/// Store the project's replication policy and return what it selects. The
/// worker is kicked, so a widened policy starts fetching without waiting for
/// the cadence. A narrowed one never deletes anything already held.
pub async fn set_collab_policy(
    ctx: &ServiceContext,
    project_id: &str,
    policy: ReplicationPolicy,
) -> Result<PolicyPreview, ApiError> {
    validate_policy(&policy)?;
    let preview = {
        let db = db(ctx)?;
        let conn = db.conn();
        live_project(&conn, project_id)?;
        let json = serde_json::to_string(&policy)
            .map_err(|e| ApiError::Internal(format!("encode replication policy: {e}")))?;
        crate::db::collab::set_policy(&conn, project_id, &json)?;
        let rows = crate::db::collab_frames::list_for_project(&conn, project_id)?;
        policy_preview(&rows, &policy)
    };
    tracing::info!(
        project_id,
        count = preview.to_fetch,
        "replication policy set"
    );
    auto_sync_kick().notify_one();
    Ok(preview)
}

/// Adapts an optional shared emitter to the scanner's `&E` parameter.
struct OptEmitter(Option<Arc<dyn ProgressEmitter>>);

impl ProgressEmitter for OptEmitter {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        if let Some(em) = &self.0 {
            em.emit_json(event_name, payload);
        }
    }
}

/// Answer a paused project (P14).
///
/// - `Restore`: rescan the Collaboration root with the same entry the
///   Rescan button uses (the scanner repairs moved files), run disk truth
///   again so the found files are re-admitted, unpause and kick the worker.
/// - `StopHolding`: decline every replica that is still missing, then
///   unpause. Nothing is deleted.
///
/// `_sync` keeps the command surface uniform with the other replication
/// commands; the resumed pass runs on the worker, kicked.
pub async fn resolve_collab_loss(
    ctx: Arc<ServiceContext>,
    _sync: Arc<crate::sync::SyncRuntime>,
    project_id: &str,
    action: LossAction,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<(), ApiError> {
    use rusqlite::OptionalExtension;
    {
        let db = db(&ctx)?;
        live_project(&db.conn(), project_id)?;
    }
    match action {
        LossAction::Restore => {
            let root_id: i64 = {
                let db = db(&ctx)?;
                let conn = db.conn();
                conn.query_row(
                    "SELECT id FROM scan_roots WHERE kind = 'collaboration' LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(|| ApiError::Invalid(COLLABORATION_ROOT_REQUIRED.to_string()))?
            };
            let scan_ctx = Arc::clone(&ctx);
            let em = OptEmitter(emitter.clone());
            tokio::task::spawn_blocking(move || {
                crate::api::scan_roots::start_scan_with_progress(&scan_ctx, root_id, &em)
            })
            .await
            .map_err(|e| ApiError::Internal(format!("restore scan task: {e}")))?
            .inspect_err(|e| {
                tracing::error!(project_id, error = %e, "restore rescan failed");
            })?;
            let truth = disk_truth(&ctx, project_id).await?;
            {
                let db = db(&ctx)?;
                crate::db::collab::set_replication_paused(&db.conn(), project_id, false)?;
            }
            tracing::info!(
                project_id,
                count = truth.present.len(),
                missing = truth.missing_replicas.len(),
                "replication resumed after restore"
            );
            auto_sync_kick().notify_one();
        }
        LossAction::StopHolding => {
            let db = db(&ctx)?;
            let conn = db.conn();
            let missing: Vec<String> =
                crate::db::collab_frames::list_for_project(&conn, project_id)?
                    .into_iter()
                    .filter(|r| {
                        r.origin == FrameOrigin::Replica
                            && !r.on_disk
                            && r.landed_path.is_some()
                            && !r.locally_declined
                    })
                    .map(|r| r.frame_uuid)
                    .collect();
            crate::db::collab_frames::set_declined(&conn, project_id, &missing, true)?;
            crate::db::collab::set_replication_paused(&conn, project_id, false)?;
            tracing::info!(
                project_id,
                count = missing.len(),
                "missing frames no longer held; replication resumed"
            );
        }
    }
    Ok(())
}

/// How often the auto-replication worker sweeps every auto-enabled project
/// (spec §3.3). Long by design: a whole-swarm failure means every holder is
/// gone, and 20 minutes is an honest retry interval for that.
pub const COLLAB_AUTO_SYNC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20 * 60);

/// Grace before the FIRST pass of a session, so auto-replication never competes
/// with app start (receiver boot, initial scan, first render). The monitor loop's
/// 3 s startup deferral is the same idea, scaled to a background bulk pull.
const COLLAB_AUTO_SYNC_STARTUP_DELAY: std::time::Duration = std::time::Duration::from_secs(90);

/// Armed once per process. The worker is spawned from EVERY `ensure_started`
/// site (autostart + the dev `start_sync`), exactly like the sender resurrection
/// and the orphan sweep, so this flag — not the call site — is what makes it
/// one loop per app run.
static AUTO_SYNC_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What one pass did — the loop's log line and the unit tests' oracle.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AutoSyncPassOutcome {
    /// Projects whose need set went to the fetch.
    pub projects: usize,
    /// Frames handed to the fetch.
    pub attempted: usize,
    /// Frames landed (fetched or linked, P24).
    pub landed: usize,
    /// Frames that failed, plus one per project whose whole fetch errored.
    pub failed: usize,
    /// Frames parked until the collab store's GC drops a dead entry (P20).
    pub awaiting_gc: usize,
    /// Full holder reports sent (cadence and forced passes only, P8).
    pub reported: usize,
}

/// May this device replicate a project's frames at all? `coordinator ||
/// data_role == "send_receive"` against the CACHED project row. The hub
/// filters holder rows by the same rule, so a stale cache costs at most one
/// fetch whose holds the hub drops.
fn role_allows_replication(data_role: &str, is_coordinator: bool) -> bool {
    is_coordinator || data_role == "send_receive"
}

/// One replication pass (spec §5.3, §5.5).
///
/// `scope` limits it to one project ("Sync now"); `None` sweeps every live
/// project (a lost project is never listed, R14). Per project, in order:
///
/// 1. `Cadence` / `Forced` only: [`disk_truth`], then the [`loss_guard`]
///    (skipped when already paused), then the FULL [`report_holders`] — for
///    every role, since a `send` member still holds its own frames. A
///    `Kicked` pass never walks the disk and never sends a full report.
/// 2. Parked frames whose store entry is gone become fetchable again.
/// 3. [`frame_need`] under the project's policy, the role gate, the toggle
///    (`Forced` turns it on; the role gate is authorization and never is) and
///    the pause.
/// 4. `fetch` — production binds [`fetch_frames`]; tests inject a recorder.
///
/// No Collaboration root ⇒ one `warn!` and every project is skipped (P25).
/// Never returns an error: signed out, an unreadable catalog, a failing
/// project are each logged and stepped over, because the caller is a loop
/// that must survive all of them.
async fn run_auto_sync_pass<F, Fut>(
    ctx: &ServiceContext,
    kind: PassKind,
    scope: Option<&str>,
    emitter: Option<&dyn ProgressEmitter>,
    fetch: F,
) -> AutoSyncPassOutcome
where
    F: Fn(String, Vec<LocalFrameRow>) -> Fut,
    Fut: std::future::Future<Output = Result<FetchOutcome, ApiError>>,
{
    let mut outcome = AutoSyncPassOutcome::default();

    match crate::api::account::hub_credentials(ctx) {
        Ok(Some(_)) => {}
        Ok(None) => {
            tracing::debug!("collab auto-sync: signed out; pass skipped");
            return outcome;
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e}"), "collab auto-sync: account read failed; pass skipped");
            return outcome;
        }
    }
    // P25: the one warn of this pass is `require_collaboration_root`'s own.
    if require_collaboration_root(ctx).is_err() {
        return outcome;
    }

    let projects = {
        let database = match db(ctx) {
            Ok(database) => database,
            Err(e) => {
                tracing::warn!(error = %format!("{e}"), "collab auto-sync: catalog unavailable; pass skipped");
                return outcome;
            }
        };
        match crate::db::collab::list_projects(&database.conn()) {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "collab auto-sync: project list failed; pass skipped");
                return outcome;
            }
        }
    };

    for project in projects {
        if scope.is_some_and(|only| only != project.project_id) {
            continue;
        }
        let pid = project.project_id.clone();
        let role_allows = role_allows_replication(&project.data_role, project.is_coordinator);
        let auto_on = kind == PassKind::Forced || project.auto_replicate;
        let mut paused = project.replication_paused;

        if kind.walks_disk() {
            let truth = match disk_truth(ctx, &pid).await {
                Ok(truth) => truth,
                Err(e) => {
                    tracing::warn!(project_id = %pid, error = %e, "collab auto-sync: disk truth failed; project skipped this pass");
                    continue;
                }
            };
            if !paused {
                match loss_guard(ctx, &pid, &truth, emitter) {
                    Ok(tripped) => paused = tripped,
                    Err(e) => {
                        tracing::warn!(project_id = %pid, error = %e, "collab auto-sync: loss guard failed; project skipped this pass");
                        continue;
                    }
                }
            }
            // The truth goes to the hub either way — a paused project still
            // stops advertising what it lost.
            match report_holders(ctx, &pid, &truth.present).await {
                Ok(_) => outcome.reported += 1,
                Err(e) => {
                    tracing::warn!(project_id = %pid, error = %e, "collab auto-sync: holder report failed")
                }
            }
        }

        if let Err(e) = recheck_awaiting_gc(ctx, &pid).await {
            tracing::warn!(project_id = %pid, error = %e, "collab auto-sync: parked-frame recheck failed");
        }

        let rows = {
            let database = match db(ctx) {
                Ok(database) => database,
                Err(e) => {
                    tracing::warn!(error = %format!("{e}"), "collab auto-sync: catalog unavailable; pass aborted");
                    return outcome;
                }
            };
            match crate::db::collab_frames::list_for_project(&database.conn(), &pid) {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!(project_id = %pid, error = %format!("{e:#}"), "collab auto-sync: frame list failed; project skipped this pass");
                    continue;
                }
            }
        };
        let need = frame_need(&rows, &read_policy(&project), role_allows, auto_on, paused);
        if need.is_empty() {
            tracing::debug!(
                project_id = %pid,
                role_allows,
                auto_on,
                paused,
                "collab auto-sync: nothing to replicate"
            );
            continue;
        }
        outcome.projects += 1;
        outcome.attempted += need.len();
        tracing::info!(project_id = %pid, count = need.len(), "collab auto-sync: replicating missing frames");
        match fetch(pid.clone(), need).await {
            Ok(f) => {
                outcome.landed += f.landed;
                outcome.failed += f.failed;
                outcome.awaiting_gc += f.awaiting_gc;
            }
            Err(e) => {
                outcome.failed += 1;
                tracing::warn!(project_id = %pid, error = %e, "collab auto-sync: replication fetch failed; continuing");
            }
        }
    }

    tracing::info!(
        pass = kind.as_str(),
        projects = outcome.projects,
        attempted = outcome.attempted,
        landed = outcome.landed,
        failed = outcome.failed,
        awaiting_gc = outcome.awaiting_gc,
        "collab auto-sync pass complete"
    );
    outcome
}

/// One pass with the REAL fetch bound (the production seam).
pub(crate) async fn replication_pass(
    ctx: &ServiceContext,
    sync: &crate::sync::SyncRuntime,
    kind: PassKind,
    scope: Option<&str>,
    emitter: Option<&dyn ProgressEmitter>,
) -> AutoSyncPassOutcome {
    run_auto_sync_pass(ctx, kind, scope, emitter, move |project_id, need| async move {
        fetch_frames(ctx, sync, &project_id, need, emitter).await
    })
    .await
}

/// [`replication_pass`] over owned handles, so the loop can run it on its
/// own task.
async fn auto_sync_pass(
    ctx: Arc<ServiceContext>,
    sync: Arc<crate::sync::SyncRuntime>,
    emitter: Option<Arc<dyn ProgressEmitter>>,
    scope: Option<String>,
    kind: PassKind,
) -> AutoSyncPassOutcome {
    replication_pass(&ctx, &sync, kind, scope.as_deref(), emitter.as_deref()).await
}

/// The auto-replication loop (spec §3.3): a pass every `interval` OR as soon as a
/// hub change arrives, after a short startup grace. Beside it, on its own
/// task, the version poll (R19, ruling R15) asks the hub every
/// [`COLLAB_VERSION_POLL_INTERVAL`] whether a project moved and only kicks
/// the pass when one did. Each pass and each poll tick runs on its own task,
/// so a panic anywhere below is logged and the loops survive it (a background
/// loop that dies is a feature that silently stops).
pub async fn run_collab_auto_sync_loop(
    ctx: Arc<ServiceContext>,
    sync: Arc<crate::sync::SyncRuntime>,
    emitter: Option<Arc<dyn ProgressEmitter>>,
    interval: std::time::Duration,
) {
    tracing::info!(
        interval_secs = interval.as_secs(),
        poll_secs = COLLAB_VERSION_POLL_INTERVAL.as_secs(),
        "collab auto-sync loop armed"
    );
    tokio::spawn(version_poll_loop(
        COLLAB_VERSION_POLL_STARTUP_DELAY,
        COLLAB_VERSION_POLL_INTERVAL,
        {
            let ctx = Arc::clone(&ctx);
            let emitter = emitter.clone();
            move || {
                let tick = tokio::spawn(version_poll_tick(Arc::clone(&ctx), emitter.clone()));
                async move {
                    if let Err(error) = tick.await {
                        tracing::error!(%error, "collab version poll task panicked");
                    }
                }
            }
        },
    ));
    auto_sync_loop_inner(
        COLLAB_AUTO_SYNC_STARTUP_DELAY.min(interval),
        interval,
        auto_sync_kick(),
        move |kind| {
            let ctx = Arc::clone(&ctx);
            let sync = Arc::clone(&sync);
            let emitter = emitter.clone();
            async move {
                let pass = tokio::spawn(auto_sync_pass(ctx, sync, emitter, None, kind));
                if let Err(error) = pass.await {
                    tracing::error!(%error, "collab auto-sync pass task panicked");
                }
            }
        },
    )
    .await
}

/// The loop's shape, with the pass and the kick injected (the production
/// binding is [`run_collab_auto_sync_loop`] with [`auto_sync_kick`]; tests
/// pass a counter and their own `Notify`).
///
/// The startup grace is deliberately NOT interruptible: it exists so bulk pulls
/// don't compete with app start (receiver boot, initial scan, first render). A
/// kick that lands during a running pass is not lost:
/// [`tokio::sync::Notify::notify_one`] stores ONE permit when nobody is waiting,
/// so the wait below returns immediately the next time round and produces
/// exactly one follow-up pass no matter how many kicks arrived. A permit
/// stored BEFORE a timer pass (e.g. a version move during the grace) is
/// drained when that pass starts — the pass about to run already covers it,
/// so it must not buy a second pass right after. Overlapping per-project work
/// between a kicked pass and a "Sync now" is prevented by the fetch's
/// per-project claim, not by the cadence.
///
/// A timer pass is [`PassKind::Cadence`] (disk truth + the full holder
/// report); a kicked one is [`PassKind::Kicked`] (manifest-driven work only,
/// spec §5.5).
async fn auto_sync_loop_inner<F, Fut>(
    startup_delay: std::time::Duration,
    interval: std::time::Duration,
    kick: &tokio::sync::Notify,
    run_pass: F,
) where
    F: Fn(PassKind) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    tokio::time::sleep(startup_delay).await;
    let mut timer_pass = true;
    loop {
        if timer_pass {
            // Polling `notified()` once consumes a stored permit, if any.
            let _ = tokio::time::timeout(std::time::Duration::ZERO, kick.notified()).await;
        }
        run_pass(if timer_pass {
            PassKind::Cadence
        } else {
            PassKind::Kicked
        })
        .await;
        tokio::select! {
            _ = tokio::time::sleep(interval) => timer_pass = true,
            _ = kick.notified() => {
                tracing::debug!("collab auto-sync: a hub change arrived; running a pass now");
                timer_pass = false;
            }
        }
    }
}

/// Arm the auto-replication loop for this process (D3 §3.3). Called from every
/// `ensure_started` site; the second and later calls are no-ops, so the app runs
/// exactly one worker. Returns the spawned handle only for the call that armed
/// it (tests / callers that want to observe it).
pub fn spawn_collab_auto_sync(
    ctx: Arc<ServiceContext>,
    sync: Arc<crate::sync::SyncRuntime>,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Option<tokio::task::JoinHandle<()>> {
    if AUTO_SYNC_ARMED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        tracing::debug!("collab auto-sync already armed; not spawning a second loop");
        return None;
    }
    Some(tokio::spawn(run_collab_auto_sync_loop(
        ctx,
        sync,
        emitter,
        COLLAB_AUTO_SYNC_INTERVAL,
    )))
}

/// Set one project's auto-replication preference (D3 §3.3). Local-only — the hub
/// never learns of it. The worker reads the column at the start of every pass,
/// so there is nothing to live-apply: turning it off stops the NEXT pass, and a
/// download already in flight is finished (it is a receive like any other).
pub fn set_project_auto_replicate(
    ctx: &ServiceContext,
    project_id: &str,
    enabled: bool,
) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let updated = crate::db::collab::set_auto_replicate(&conn, project_id, enabled)?;
    if updated == 0 {
        return Err(ApiError::Invalid(format!("unknown project {project_id}")));
    }
    tracing::info!(project_id, enabled, "collab auto-replication toggled");
    Ok(())
}

/// "Sync now" for one project: the version poll (so the pass works from the
/// current manifest), then ONE [`PassKind::Forced`] pass scoped to
/// `project_id`, on a spawned task — the command returns as soon as the
/// project is known. The pass walks the disk, reports holders and fetches
/// with the auto-replicate toggle forced on (an explicit user act); the role
/// gate is authorization and is never forced. Outcomes ride the pass's
/// events (`collab-frames-landed`, `collab-replication-paused`).
///
/// A project the poll moved besides this one gets the usual kick, since this
/// pass only covers `project_id`.
pub fn sync_project_now(
    ctx: Arc<ServiceContext>,
    sync: Arc<crate::sync::SyncRuntime>,
    project_id: &str,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<(), ApiError> {
    {
        let db = db(&ctx)?;
        live_project(&db.conn(), project_id)?;
    }
    tracing::info!(project_id, "collab sync now requested");
    let scope = project_id.to_string();
    tokio::spawn(async move {
        #[cfg(all(feature = "render", feature = "solver"))]
        match poll_versions_once(&ctx, emitter.as_deref()).await {
            Ok(moved) => {
                if moved.iter().any(|id| id != &scope) {
                    kick_if_versions_moved(&moved);
                }
            }
            Err(e) => {
                tracing::warn!(project_id = %scope, error = %e, "sync now: version poll failed; syncing from the cached manifest")
            }
        }
        #[cfg(not(all(feature = "render", feature = "solver")))]
        {
            // Headless: no project refresh, so no poll; the manifest delta
            // alone (logged inside on failure).
            let _ = sync_manifest(&ctx, &scope, emitter.as_deref(), None).await;
        }
        auto_sync_pass(ctx, sync, emitter, Some(scope), PassKind::Forced).await;
    });
    Ok(())
}

// ── Project-scoped WBPP export (slice 5, "processor payoff") ──────────────────

/// Load the WBPP config from settings, or the default when absent. A private
/// reproduction of the byte-identical `load_wbpp_config` in both host crates
/// (`commands/export.rs` / `routes/export.rs`) — the runner is core-resident and
/// must not depend on a host. Absent row ⇒ default; a parse failure ⇒ `warn!` +
/// `Err` (callers use `.unwrap_or_default()`, exactly like both hosts).
fn load_wbpp_config(conn: &rusqlite::Connection) -> Result<WbppExportConfig> {
    let result: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params!["export.wbpp_config"],
            |row| row.get(0),
        )
        .ok();
    match result {
        Some(json) => serde_json::from_str(&json).map_err(|e| {
            tracing::warn!(error = %e, "failed to parse WBPP config");
            anyhow!(e)
        }),
        None => Ok(WbppExportConfig::default()),
    }
}

/// The project export: collect (Task 1) → organize one folder tree per publisher
/// under `<output_dir>/<sanitized project title>/`, with each dataset's
/// `frame_set_name` = the publisher display (Д2 — one organizer call per
/// publisher). Rides the standard export events with the Д3 sentinel
/// `frame_set_id = -1`, registering its cancel flag under that key exactly like
/// the frame-set export.
///
/// Ungated: the collector is a pure catalog read and the organizer is reused
/// untouched, so this compiles in the headless build.
///
/// Progress limitation (accepted): each organizer call emits percent against ITS
/// OWN dataset total, so the bar restarts per publisher — the Task-2 dialog
/// shows current file + publisher count, not one monotonic percent.
pub async fn export_project_for_wbpp(
    ctx: &ServiceContext,
    project_id: &str,
    output_dir: &str,
    use_symlinks: bool,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<crate::export::models::ExportResult, ApiError> {
    use crate::export::models::{
        sanitize_display_folder_name, ExportCompleteEvent, ExportProgressEvent, ExportResult,
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    const SENTINEL: i64 = -1;

    // Register the cancel flag under the sentinel (the registry is core-resident).
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut exports = ctx
            .active_exports
            .lock()
            .map_err(|e| ApiError::Internal(format!("active_exports lock poisoned: {e}")))?;
        exports.insert(
            SENTINEL,
            crate::services::ExportHandle {
                cancel_flag: cancel_flag.clone(),
            },
        );
    }

    // The whole export runs inside a scoped block so the terminal event AND the
    // deregister below fire on every path (success, collector error, organize
    // error) — never swallowed.
    let outcome: Result<ExportResult, ApiError> = async {
        if let Some(e) = emitter.as_deref() {
            crate::events::emit_event(
                e,
                "export-progress",
                &ExportProgressEvent {
                    frame_set_id: SENTINEL,
                    current: 0,
                    total: 0,
                    percent: 0.0,
                    current_file: None,
                    phase: "collecting".to_string(),
                },
            );
        }

        // Resolve own_display (Д2) + the WBPP config with the DB borrow scoped out
        // before the spawn_blocking/await.
        // The device key is IDENTITY — always the identity dir.
        let identity_dir = crate::api::sync::sync_dirs(ctx)?.identity_dir;
        let own_node = DeviceKey::load_or_create(&device_key_path(&identity_dir))
            .map_err(|e| ApiError::Internal(format!("device key: {e:#}")))?
            .node_id();
        let (own_display, config) = {
            let db = db(ctx)?;
            let conn = db.conn();
            let members: Vec<SnapshotMember> = crate::db::collab::get_project(&conn, project_id)?
                .map(|p| serde_json::from_str(&p.members_json).unwrap_or_default())
                .unwrap_or_default();
            let mut display = own_display_name(&members, &own_node);
            if display.is_empty() {
                tracing::warn!(
                    project_id,
                    "project export: could not resolve own display name from snapshot; using \"own\""
                );
                display = "own".to_string();
            }
            (display, load_wbpp_config(&conn).unwrap_or_default())
        };

        // Collect on a blocking thread (pure catalog read; own DB handle).
        let data = {
            let db_handle = db(ctx)?.clone();
            let pid = project_id.to_string();
            let own = own_display.clone();
            tokio::task::spawn_blocking(
                move || -> Result<crate::export::ProjectExportData> {
                    let conn = db_handle.conn();
                    crate::export::collect_project_export_data(&conn, &pid, &own)
                },
            )
            .await
            .map_err(|e| ApiError::Internal(format!("collect join error: {e}")))??
        };

        // <output_dir>/<sanitized project title>/, then one organizer call per
        // publisher (the organizer joins the publisher folder itself).
        let title_dir = Path::new(output_dir).join(sanitize_display_folder_name(&data.title));
        let mut files_organized = 0i32;
        // Prepend the collector's per-skip notes (own light with no calibrated
        // output, missing output on disk, unreadable contribution metadata) so
        // omitted frames reach ExportResult.warnings — and the dialog — rather
        // than vanishing behind a smaller "N files organized" count.
        let mut warnings: Vec<String> = data.warnings.clone();
        let mut cancelled = false;
        for (publisher, dataset) in &data.publishers {
            if cancel_flag.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let result = crate::export::file_organizer::organize_files_wbpp(
                &title_dir,
                dataset,
                use_symlinks,
                &config,
                emitter.as_deref(),
                SENTINEL,
                &cancel_flag,
                // Collab datasets are always copied — the contributions arrive
                // already-calibrated or raw, never marked for generation.
                None,
                Some(&ctx.image_pool),
            )
            .map_err(|e| ApiError::Internal(format!("organize publisher {publisher}: {e:#}")))?;
            files_organized += result.files_organized;
            warnings.extend(result.warnings);
        }
        if cancelled || cancel_flag.load(Ordering::Relaxed) {
            return Ok(ExportResult {
                success: false,
                output_dir: output_dir.to_string(),
                files_organized,
                scripts_generated: Vec::new(),
                warnings,
                error: Some("Export cancelled".to_string()),
            });
        }

        Ok(ExportResult {
            success: true,
            output_dir: output_dir.to_string(),
            files_organized,
            scripts_generated: Vec::new(),
            warnings,
            error: None,
        })
    }
    .await;

    // Deregister on every path.
    if let Ok(mut exports) = ctx.active_exports.lock() {
        exports.remove(&SENTINEL);
    }

    // Terminal event (success or failure), never swallowed.
    if let Some(e) = emitter.as_deref() {
        let complete = match &outcome {
            Ok(r) => ExportCompleteEvent {
                frame_set_id: SENTINEL,
                success: r.success,
                files_organized: r.files_organized,
                warnings: r.warnings.clone(),
                error: r.error.clone(),
                output_dir: output_dir.to_string(),
            },
            Err(err) => ExportCompleteEvent {
                frame_set_id: SENTINEL,
                success: false,
                files_organized: 0,
                warnings: Vec::new(),
                error: Some(err.to_string()),
                output_dir: output_dir.to_string(),
            },
        };
        crate::events::emit_event(e, "export-complete", &complete);
    }

    match &outcome {
        Ok(r) => tracing::info!(
            project_id,
            files_organized = r.files_organized,
            outcome = if r.success { "ok" } else { "cancelled" },
            "project export finished"
        ),
        Err(err) => tracing::error!(project_id, error = %err, "project export failed"),
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::collab::{upsert_project, CollabProjectRow};
    use crate::db::collab_exchange::{
        contributions_for_package, insert_contribution, upsert_package, PackageRow,
    };
    use crate::package::{ManifestRecord, PayloadKind, ProjectStamp, MANIFEST_VERSION};
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    use rusqlite::Connection;
    use std::time::Duration;

    const NODE_COORD: NodeId = [0xA1; 32]; // coordinator + send_receive
    const NODE_SEND_ONLY: NodeId = [0xB2; 32]; // send-only contributor
    const NODE_SR: NodeId = [0xC3; 32]; // send_receive, non-coordinator
    const STRANGER: NodeId = [0xD4; 32]; // not a member

    /// In-memory catalog with the full schema + FK enforcement.
    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn
    }

    fn hash_bytes(bytes: &[u8]) -> String {
        format!("{:016x}", xxhash_rust::xxh3::xxh3_64(bytes))
    }

    /// A three-member snapshot (encoded exactly as slice-3 caches it): a
    /// coordinator+`send_receive` (NODE_COORD), a send-only contributor
    /// (NODE_SEND_ONLY), and a non-coordinator `send_receive` (NODE_SR).
    fn members_json() -> String {
        serde_json::json!([
            {"accountId":"acc-coord","displayName":"Coord","dataRole":"send_receive","coordinator":true,"nodes":[B64.encode(NODE_COORD)]},
            {"accountId":"acc-so","displayName":"SendOnly","dataRole":"send","coordinator":false,"nodes":[B64.encode(NODE_SEND_ONLY)]},
            {"accountId":"acc-sr","displayName":"SendRecv","dataRole":"send_receive","coordinator":false,"nodes":[B64.encode(NODE_SR)]}
        ])
        .to_string()
    }

    fn seed_project(conn: &Connection, project_id: &str, members_json: &str) {
        upsert_project(
            conn,
            &CollabProjectRow {
                project_id: project_id.to_string(),
                slug: format!("{project_id}-slug"),
                title: "T".to_string(),
                data_role: "send_receive".to_string(),
                is_coordinator: true,
                require_approval: false,
                pending_frames: 0,
                project_status: "active".to_string(),
                target_name: "M42".to_string(),
                target_ra_deg: 83.8,
                target_dec_deg: -5.4,
                target_radius_deg: 1.0,
                membership_version: 1,
                snapshot_payload_b64: "x".to_string(),
                snapshot_signature_b64: "x".to_string(),
                members_json: members_json.to_string(),
                thresholds_version: None,
                thresholds_rules_json: None,
                gov_caps_json: "[]".into(),
                // all ignored on write (local preference / sync-state / dictionary)
                auto_replicate: true,
                synced_caps_json: "[]".into(),
                hub_version: 0,
                manifest_cursor: 0,
                dictionary_version: None,
                dictionary_json: None,
                policy_json: r#"{"mode":"all"}"#.into(),
                replication_paused: false,
                auto_publish: true,
                fetched_at: String::new(),
            },
        )
        .unwrap();
    }

    /// Default remote package row (tests mutate the fields they care about).
    fn base_package(hub: &str, project: &str, publisher: &str) -> PackageRow {
        PackageRow {
            package_id: hub.to_string(),
            project_id: project.to_string(),
            announcement_id: format!("ann-{hub}"),
            publisher_display: publisher.to_string(),
            own: false,
            root_hash: "rh".to_string(),
            byte_size: 0,
            frame_count: 1,
            manifest_xxh3: None,
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
        }
    }

    /// Build the NDJSON manifest bytes + anchor for a one-frame stamped package.
    fn one_frame_manifest(
        rel_path: &str,
        payload: &[u8],
        project_id: &str,
        hub_package_id: &str,
    ) -> (Vec<u8>, String, ManifestRecord) {
        let rec = ManifestRecord {
            v: MANIFEST_VERSION,
            frame_uuid: "u-1".to_string(),
            origin_catalog_uuid: "cat".to_string(),
            origin_device: "de".repeat(32),
            payload_kind: PayloadKind::CalibratedLight,
            rel_path: rel_path.to_string(),
            byte_size: payload.len() as u64,
            xxh3: hash_bytes(payload),
            frame_meta: serde_json::json!({ "object": "M42" }),
            analysis: None,
            app_version: "test".to_string(),
            project: Some(ProjectStamp {
                project_id: project_id.to_string(),
                package_id: hub_package_id.to_string(),
                thresholds_version: None,
                cal_engine_version: None,
            }),
        };
        let mut buf = String::new();
        buf.push_str(&serde_json::to_string(&rec).unwrap());
        buf.push('\n');
        let bytes = buf.into_bytes();
        let anchor = hash_bytes(&bytes);
        (bytes, anchor, rec)
    }

    /// Seed a received-complete package: writes the landed payload on disk at
    /// `<landing>/<hub>/L_0001.fits`, upserts the package row (origin=received,
    /// retained manifest, given `state`, local_status=complete) + one contribution
    /// row pointing at the landed file. Returns the retained manifest bytes.
    fn seed_received_package(
        conn: &Connection,
        landing: &Path,
        project_id: &str,
        hub: &str,
        publisher: &str,
        state: &str,
        payload: &[u8],
    ) -> Vec<u8> {
        let rel_path = "L_0001.fits";
        let (manifest_bytes, anchor, rec) = one_frame_manifest(rel_path, payload, project_id, hub);
        let landed = landing.join(hub).join(rel_path);
        std::fs::create_dir_all(landed.parent().unwrap()).unwrap();
        std::fs::write(&landed, payload).unwrap();

        let mut row = base_package(hub, project_id, publisher);
        row.origin = "received".to_string();
        row.state = state.to_string();
        row.local_status = "complete".to_string();
        row.manifest_xxh3 = Some(anchor);
        row.manifest_ndjson = Some(manifest_bytes.clone());
        row.byte_size = payload.len() as i64;
        upsert_package(conn, &row).unwrap();

        insert_contribution(
            conn,
            &ContributionRow {
                id: 0,
                project_id: project_id.to_string(),
                package_id: hub.to_string(),
                frame_uuid: rec.frame_uuid.clone(),
                publisher_display: publisher.to_string(),
                rel_path: rel_path.to_string(),
                landed_path: landed.to_string_lossy().to_string(),
                byte_size: payload.len() as i64,
                xxh3: rec.xxh3.clone(),
                frame_meta: "{}".to_string(),
                analysis: None,
                superseded: false,
                created_at: String::new(),
            },
        )
        .unwrap();
        manifest_bytes
    }

    // ── Slice 5 Task 1: project export runner ────────────────────────────────

    /// The project export runner lays a per-publisher WBPP tree under the project
    /// title and copies the received contribution's landed FITS byte-exact:
    /// `<out>/<title>/<publisher>/camera_<instrume>/lights/<basename>`.
    #[tokio::test]
    async fn export_project_lays_per_publisher_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));

        // A real tiny FITS as the landed contribution file.
        let landed = tmp.path().join("land").join("Alice").join("L_0001.fits");
        std::fs::create_dir_all(landed.parent().unwrap()).unwrap();
        let pixels = vec![0.25f32, 0.5, 0.75, 1.0];
        crate::fits_writer::write_fits_f32(&landed, 2, 2, 1, &pixels, &[]).unwrap();

        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            seed_project(&conn, "p-1", "[]");
            upsert_package(&conn, &base_package("hub-1", "p-1", "Alice")).unwrap();
            insert_contribution(
                &conn,
                &ContributionRow {
                    id: 0,
                    project_id: "p-1".into(),
                    package_id: "hub-1".into(),
                    frame_uuid: "u-1".into(),
                    publisher_display: "Alice".into(),
                    rel_path: "Alice/L_0001.fits".into(),
                    landed_path: landed.to_string_lossy().to_string(),
                    byte_size: 1,
                    xxh3: "h".into(),
                    frame_meta: r#"{"instrume":"CamA","filter":"L","exptime":300.0}"#.into(),
                    analysis: None,
                    superseded: false,
                    created_at: String::new(),
                },
            )
            .unwrap();
        }

        let out = tmp.path().join("out");
        let result = export_project_for_wbpp(&ctx, "p-1", &out.to_string_lossy(), false, None)
            .await
            .unwrap();
        assert!(result.success, "export succeeded: {:?}", result.error);
        assert_eq!(result.files_organized, 1, "one received frame organized");

        // seed_project titles the project "T"; instrume "CamA" → camera_cama.
        let dest = out
            .join("T")
            .join("Alice")
            .join("camera_cama")
            .join("lights")
            .join("L_0001.fits");
        assert!(dest.exists(), "expected {dest:?}");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            std::fs::read(&landed).unwrap(),
            "the organized copy is byte-identical to the landed contribution"
        );
    }

    // ── Step 1: reconstruct_serve_dir ────────────────────────────────────────

    /// A received package materializes a serve dir with a byte-identical manifest
    /// and payloads hard-linked from the landed files.
    #[cfg(unix)]
    #[test]
    fn reconstruct_received_is_byte_identical_and_hardlinked() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        let landing = tmp.path().join("land");
        let payload = b"the-frame-payload-bytes";
        let manifest_bytes = seed_received_package(
            &conn,
            &landing,
            "p-1",
            "hub-1",
            "Alice",
            "published",
            payload,
        );

        let sync_dir = tmp.path().join("sync");
        let dir = reconstruct_serve_dir(&conn, &sync_dir, "hub-1").unwrap();
        assert_eq!(dir, sync_dir.join("collab_serve").join("hub-1"));

        // Manifest byte-exact.
        let got = std::fs::read(dir.join(crate::package::MANIFEST_FILENAME)).unwrap();
        assert_eq!(
            got, manifest_bytes,
            "manifest.ndjson byte-identical to the retained bytes"
        );

        // Payload present, correct content, and HARD-LINKED to the landed file.
        let served = dir.join("L_0001.fits");
        assert_eq!(std::fs::read(&served).unwrap(), payload);
        let landed = landing.join("hub-1").join("L_0001.fits");
        let m_served = std::fs::metadata(&served).unwrap();
        let m_landed = std::fs::metadata(&landed).unwrap();
        assert_eq!(
            m_served.ino(),
            m_landed.ino(),
            "serve payload is a hard link (same inode)"
        );
        assert_eq!(m_served.dev(), m_landed.dev());
        assert!(m_landed.nlink() >= 2, "landed file now has >=2 links");

        // The reconstructed dir validates against its own manifest end to end.
        crate::package::validate_package(&dir).unwrap();
    }

    /// A second reconstruct call is idempotent: same dir, payload untouched.
    #[cfg(unix)]
    #[test]
    fn reconstruct_second_call_is_idempotent() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        let landing = tmp.path().join("land");
        let payload = b"payload-xyz";
        seed_received_package(
            &conn,
            &landing,
            "p-1",
            "hub-1",
            "Alice",
            "published",
            payload,
        );
        let sync_dir = tmp.path().join("sync");

        let dir1 = reconstruct_serve_dir(&conn, &sync_dir, "hub-1").unwrap();
        let ino1 = std::fs::metadata(dir1.join("L_0001.fits")).unwrap().ino();
        let dir2 = reconstruct_serve_dir(&conn, &sync_dir, "hub-1").unwrap();
        assert_eq!(dir1, dir2);
        let ino2 = std::fs::metadata(dir2.join("L_0001.fits")).unwrap().ino();
        assert_eq!(
            ino1, ino2,
            "idempotent — payload untouched on the second call"
        );
        crate::package::validate_package(&dir2).unwrap();
    }

    /// origin='mine' returns the retained local_dir as-is and never materializes a
    /// collab_serve dir.
    #[test]
    fn reconstruct_mine_returns_local_dir_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        let pub_dir = tmp.path().join("collab_pub").join("hub-mine");
        std::fs::create_dir_all(&pub_dir).unwrap();
        std::fs::write(
            pub_dir.join(crate::package::MANIFEST_FILENAME),
            b"retained-manifest\n",
        )
        .unwrap();

        let mut row = base_package("hub-mine", "p-1", "Me");
        row.own = true;
        row.origin = "mine".to_string();
        row.local_dir = Some(pub_dir.to_string_lossy().to_string());
        upsert_package(&conn, &row).unwrap();

        let sync_dir = tmp.path().join("sync");
        let dir = reconstruct_serve_dir(&conn, &sync_dir, "hub-mine").unwrap();
        assert_eq!(dir, pub_dir, "origin=mine returns the retained local_dir");
        assert!(
            !sync_dir.join("collab_serve").exists(),
            "mine never materializes a serve dir"
        );
        assert_eq!(
            std::fs::read(pub_dir.join(crate::package::MANIFEST_FILENAME)).unwrap(),
            b"retained-manifest\n",
            "local_dir left untouched"
        );
    }

    // ── Step 1: CollabCleanupSink ────────────────────────────────────────────

    /// The sink cleans a reconstructed `collab_serve` dir on terminal but refuses
    /// a retained `collab_pub` publication (Д4). Idempotent on an already-gone dir.
    #[test]
    fn cleanup_sink_deletes_serve_but_never_pub() {
        let tmp = tempfile::tempdir().unwrap();
        let serve = tmp.path().join("collab_serve").join("pkg-x");
        let pubd = tmp.path().join("collab_pub").join("pkg-x");
        for d in [&serve, &pubd] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("manifest.ndjson"), b"x").unwrap();
        }
        let sink = CollabCleanupSink;
        sink.on_terminal(&serve);
        sink.on_terminal(&pubd);
        assert!(
            !serve.exists(),
            "a reconstructed collab_serve dir is cleaned on terminal"
        );
        assert!(
            pubd.exists(),
            "a retained collab_pub publication survives (Д4)"
        );
        // Idempotent — a second terminal on the already-gone serve dir is a no-op.
        sink.on_terminal(&serve);
    }

    // ── Step 3: authorize_and_reconstruct_serve decision matrix ──────────────

    /// A published package is served to any `send_receive` member or the
    /// coordinator; a send-only contributor and a stranger are refused.
    #[test]
    fn authorize_published_allows_sr_and_coordinator_refuses_send_only() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        seed_project(&conn, "p-1", &members_json());
        let landing = tmp.path().join("land");
        seed_received_package(
            &conn,
            &landing,
            "p-1",
            "hub-1",
            "Alice",
            "published",
            b"payload",
        );
        let sync_dir = tmp.path().join("sync");

        let auth = |node: &NodeId| {
            authorize_and_reconstruct_serve(&conn, &sync_dir, node, "p-1", "hub-1").unwrap()
        };
        assert!(auth(&NODE_COORD).is_some(), "coordinator is served");
        assert!(
            auth(&NODE_SR).is_some(),
            "send_receive member is served a published package"
        );
        assert!(
            auth(&NODE_SEND_ONLY).is_none(),
            "send-only contributor is refused"
        );
        assert!(auth(&STRANGER).is_none(), "a non-member is refused");
    }

    /// A still-pending package is served ONLY to the coordinator.
    #[test]
    fn authorize_pending_is_coordinator_only() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        seed_project(&conn, "p-1", &members_json());
        let landing = tmp.path().join("land");
        seed_received_package(
            &conn, &landing, "p-1", "hub-p", "Alice", "pending", b"payload",
        );
        let sync_dir = tmp.path().join("sync");

        let auth = |node: &NodeId| {
            authorize_and_reconstruct_serve(&conn, &sync_dir, node, "p-1", "hub-p").unwrap()
        };
        assert!(
            auth(&NODE_COORD).is_some(),
            "pending → coordinator is served"
        );
        assert!(
            auth(&NODE_SR).is_none(),
            "pending → send_receive non-coordinator refused"
        );
        assert!(
            auth(&NODE_SEND_ONLY).is_none(),
            "pending → send-only refused"
        );
    }

    /// Unknown / incomplete / project-mismatch requests are refused as Ok(None).
    #[test]
    fn authorize_refuses_unknown_incomplete_and_project_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        seed_project(&conn, "p-1", &members_json());
        let sync_dir = tmp.path().join("sync");
        let landing = tmp.path().join("land");

        // Unknown package row.
        assert!(
            authorize_and_reconstruct_serve(&conn, &sync_dir, &NODE_COORD, "p-1", "nope")
                .unwrap()
                .is_none()
        );

        // Received but NOT complete.
        seed_received_package(
            &conn,
            &landing,
            "p-1",
            "hub-inc",
            "Alice",
            "published",
            b"payload",
        );
        crate::db::collab_exchange::set_local_status(&conn, "hub-inc", "downloading").unwrap();
        assert!(
            authorize_and_reconstruct_serve(&conn, &sync_dir, &NODE_COORD, "p-1", "hub-inc")
                .unwrap()
                .is_none()
        );

        // Complete, but the request names a DIFFERENT project than the package's.
        seed_received_package(
            &conn,
            &landing,
            "p-1",
            "hub-ok",
            "Alice",
            "published",
            b"payload2",
        );
        assert!(authorize_and_reconstruct_serve(
            &conn,
            &sync_dir,
            &NODE_COORD,
            "p-OTHER",
            "hub-ok"
        )
        .unwrap()
        .is_none());
        // Sanity: the same package with the correct project id is served.
        assert!(
            authorize_and_reconstruct_serve(&conn, &sync_dir, &NODE_COORD, "p-1", "hub-ok")
                .unwrap()
                .is_some()
        );
    }

    /// A refused request through `handle_project_request` returns Ok and never
    /// starts a collab sender engine (the refusal short-circuits before any
    /// transport build) — the "send-only node silently refused" case, hermetic.
    #[tokio::test]
    async fn refused_request_starts_no_collab_engine() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            seed_project(&conn, "p-1", &members_json());
            seed_received_package(
                &conn,
                &tmp.path().join("land"),
                "p-1",
                "hub-1",
                "Alice",
                "published",
                b"payload",
            );
        }
        let sender = SyncSenderRuntime::new();
        // A send-only contributor's request is silently refused.
        handle_project_request(
            &ctx,
            &sender,
            NODE_SEND_ONLY,
            "p-1".into(),
            "hub-1".into(),
            None,
        )
        .await
        .unwrap();
        assert!(
            !sender.is_started().await,
            "a refused request never starts a collab engine"
        );
        assert!(sender.started_peers().await.is_empty());
    }

    // ── Step 2: cache-only list views ────────────────────────────────────────

    /// `list_project_packages` maps every `project_packages` row (cache-only,
    /// newest-announcement-first) including the Task-3 holder/online swarm counts,
    /// and scopes to the requested project.
    #[test]
    fn list_project_packages_projects_rows_with_swarm_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            let mut p1 = base_package("hub-1", "p-1", "Alice");
            p1.created_at = "2026-07-10 00:00:00".into();
            p1.holder_count = 3;
            p1.online_count = 2;
            p1.state = "published".into();
            upsert_package(&conn, &p1).unwrap();
            let mut p2 = base_package("hub-2", "p-1", "Bob");
            p2.created_at = "2026-07-12 00:00:00".into();
            p2.state = "pending".into();
            upsert_package(&conn, &p2).unwrap();
            // A different project must not leak into the list.
            upsert_package(&conn, &base_package("hub-x", "p-OTHER", "Zed")).unwrap();
        }

        let views = list_project_packages(&ctx, "p-1").unwrap();
        assert_eq!(
            views
                .iter()
                .map(|v| v.package_id.as_str())
                .collect::<Vec<_>>(),
            vec!["hub-2", "hub-1"],
            "newest announcement first, scoped to p-1"
        );
        let hub1 = views.iter().find(|v| v.package_id == "hub-1").unwrap();
        assert_eq!(hub1.holder_count, 3, "Task-3 holder count surfaced");
        assert_eq!(hub1.online_count, 2, "Task-3 online count surfaced");
        assert_eq!(hub1.publisher, "Alice");
        assert_eq!(hub1.state, "published");
    }

    /// `list_contributions` returns every received frame for a project (oldest
    /// first), projected down to the view shape.
    #[test]
    fn list_contributions_returns_received_frames() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        {
            let db = db(&ctx).unwrap();
            let conn = db.conn();
            upsert_package(&conn, &base_package("hub-1", "p-1", "Alice")).unwrap();
            insert_contribution(
                &conn,
                &ContributionRow {
                    id: 0,
                    project_id: "p-1".into(),
                    package_id: "hub-1".into(),
                    frame_uuid: "u-1".into(),
                    publisher_display: "Alice".into(),
                    rel_path: "Alice/u-1.fits".into(),
                    landed_path: "/land/u-1.fits".into(),
                    byte_size: 2048,
                    xxh3: "hh".into(),
                    frame_meta: "{}".into(),
                    analysis: None,
                    superseded: false,
                    created_at: String::new(),
                },
            )
            .unwrap();
        }

        let views = list_contributions(&ctx, "p-1").unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].frame_uuid, "u-1");
        assert_eq!(views[0].publisher, "Alice");
        assert_eq!(views[0].byte_size, 2048);
        assert!(
            !views[0].created_at.is_empty(),
            "created_at defaulted by SQL"
        );
        assert!(list_contributions(&ctx, "p-none").unwrap().is_empty());
    }

    // ── Step 3: loopback serve e2e ───────────────────────────────────────────

    async fn wait_until<F: FnMut() -> bool>(mut pred: F, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if pred() {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("wait_until timed out after {timeout:?}");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// End-to-end (loopback): A holds a received-complete package, reconstructs the
    /// serve dir, and enqueues it to B through a `spawn_with_sink_and_emitter`
    /// collab engine. The stamped manifest routes the engine through
    /// `announce_project` (no Offer/Want); B (project gate authorizes A, hub row
    /// pre-seeded) fetches, ingests the contribution (never files/frames), and
    /// acks — A's outbound row confirms and its reconstructed serve dir is cleaned
    /// while the landed source survives.
    #[cfg(unix)]
    #[tokio::test]
    async fn served_package_lands_on_b_and_confirms_on_a() {
        use crate::sharing::loopback::{LoopbackNetwork, LoopbackTransport};
        use crate::sync::{
            allow_all_peers, OutboundState, ProjectAnnounceGate, ProjectReceiveHooks,
            StandaloneSyncStore, SyncReceiver,
        };

        const PROJECT_ID: &str = "proj-serve";
        const HUB: &str = "hub-serve-1";
        const PUBLISHER: &str = "Alice Serve";

        let tmp = tempfile::tempdir().unwrap();
        let payload = b"served-frame-bytes-0001";

        // A: seed a received-complete package + reconstruct the serve dir.
        let a_db = crate::db::Database::new(tmp.path().join("a_catalog.db")).unwrap();
        let a_landing = tmp.path().join("a_land");
        let manifest_bytes = {
            let c = a_db.conn();
            seed_received_package(
                &c,
                &a_landing,
                PROJECT_ID,
                HUB,
                PUBLISHER,
                "published",
                payload,
            )
        };
        let anchor = hash_bytes(&manifest_bytes);
        let a_sync_dir = tmp.path().join("a_sync");
        let serve_dir = {
            let c = a_db.conn();
            reconstruct_serve_dir(&c, &a_sync_dir, HUB).unwrap()
        };

        // B: seed the project (slug) + the hub-anchored package row + collab root.
        let b_catalog = tmp.path().join("b_catalog.db");
        let b_db = crate::db::Database::new(b_catalog.clone()).unwrap();
        let b_landing = tmp.path().join("b_land");
        {
            let c = b_db.conn();
            seed_project(&c, PROJECT_ID, "[]");
            let mut row = base_package(HUB, PROJECT_ID, PUBLISHER);
            row.manifest_xxh3 = Some(anchor.clone());
            upsert_package(&c, &row).unwrap();
            c.execute(
                "INSERT INTO scan_roots (path, kind) VALUES (?1, 'collaboration')",
                [b_landing.to_string_lossy().to_string()],
            )
            .unwrap();
        }

        // Loopback: A's collab engine ↔ B's receiver.
        let net = LoopbackNetwork::new();
        let a_ep = net.endpoint();
        let a_node = a_ep.node_id();
        let b_ep: Arc<LoopbackTransport> = Arc::new(net.endpoint());
        let b_node = b_ep.node_id();

        // B receiver — project gate authorizes A for this project.
        let b_store = Arc::new(CatalogSyncStore::open(&b_catalog).unwrap());
        let gate: ProjectAnnounceGate =
            Arc::new(move |from: &NodeId, pid: &str| *from == a_node && pid == PROJECT_ID);
        let b_incoming = tmp.path().join("b_incoming_unused");
        let (_info, _b_handle) = SyncReceiver::spawn(
            Arc::clone(&b_store),
            tmp.path().join("b_stage"),
            Arc::new(move || b_incoming.clone()),
            allow_all_peers(),
            ProjectReceiveHooks {
                gate: Some(gate),
                ..Default::default()
            },
            Arc::new(crate::sync::InboundControl::new()),
            Arc::clone(&b_ep) as Arc<dyn SharingTransport>,
            Arc::new(crate::events::NullEmitter),
        )
        .await
        .unwrap();

        // A collab engine targeting B, with the CollabCleanupSink.
        let a_store = Arc::new(StandaloneSyncStore::open(tmp.path().join("a_sync.db")).unwrap());
        let a_engine = SyncEngine::spawn_with_sink_and_emitter(
            Arc::clone(&a_store) as Arc<dyn SyncStore>,
            Arc::new(a_ep) as Arc<dyn SharingTransport>,
            b_node,
            Arc::new(CollabCleanupSink),
            None,
        );

        let id = a_engine
            .enqueue_package(&serve_dir, None, Vec::new(), PackageLayout::Batch)
            .await
            .unwrap();
        wait_until(
            || {
                a_store.get_outbound(id).ok().flatten().map(|r| r.state)
                    == Some(OutboundState::Confirmed)
            },
            Duration::from_secs(5),
        )
        .await;

        // B landed exactly one contribution (never into files/frames) with the
        // served bytes.
        let rows = {
            let c = b_db.conn();
            assert_eq!(
                count(&c, "SELECT COUNT(*) FROM files"),
                0,
                "contributions never enter files"
            );
            assert_eq!(
                count(&c, "SELECT COUNT(*) FROM frames"),
                0,
                "contributions never enter frames"
            );
            contributions_for_package(&c, HUB).unwrap()
        };
        assert_eq!(rows.len(), 1, "B landed the served frame as a contribution");
        assert_eq!(
            std::fs::read(&rows[0].landed_path).unwrap(),
            payload,
            "B's landed bytes match the served payload"
        );

        // The reconstructed serve dir is cleaned on terminal, but A's original
        // landed source survives (hard link — one link dropped).
        wait_until(|| !serve_dir.exists(), Duration::from_secs(5)).await;
        assert!(
            a_landing.join(HUB).join("L_0001.fits").exists(),
            "A's landed source is retained after the serve dir is cleaned"
        );

        a_engine.shutdown().await;
    }

    // ── Task 8: announcements poll + download orchestration ───────────────────

    /// A minimal file-backed-`Database` [`ServiceContext`] (no keychain), copied
    /// from `api::sync` / `api::collab` tests. A tempdir-FILE-backed `Database`
    /// (not `:memory:`) so the pool + the receiver's own `CatalogSyncStore` see
    /// one catalog file.
    fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
        use crate::cache::MemoryImageCache;
        use crate::services::compute_queue::ComputeQueue;
        use crate::services::operation_queue::OperationQueue;
        use crate::settings::SettingsManager;
        use std::collections::HashMap;
        #[cfg(all(feature = "render", feature = "solver"))]
        use std::sync::RwLock;
        use std::sync::{Mutex, OnceLock};

        let tmp = tempfile::tempdir().unwrap();
        let database = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        let db_cell = OnceLock::new();
        let _ = db_cell.set(database);
        let ctx = ServiceContext {
            db: db_cell,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(all(feature = "render", feature = "solver"))]
            active_stacks: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(all(feature = "render", feature = "solver"))]
            dso_catalog: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap(),
            ),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        };
        (tmp, ctx)
    }

    /// Point `ctx`'s account hub at `uri` + store a device token (mirrors
    /// `api::collab::wire_hub`).
    fn wire_hub(ctx: &ServiceContext, uri: &str) {
        {
            let conn = db(ctx).unwrap().conn();
            crate::db::set_setting(&conn, crate::settings::keys::ACCOUNT_HUB_URL, uri).unwrap();
        }
        crate::api::account::store_token_for_test(ctx, "tok").unwrap();
    }

    /// This device's node id for `ctx`'s sync dir — the identity the download
    /// role guard resolves against the cached membership snapshot.
    fn own_node_for(ctx: &ServiceContext) -> NodeId {
        let identity_dir = crate::api::sync::sync_dirs(ctx).unwrap().identity_dir;
        DeviceKey::load_or_create(&device_key_path(&identity_dir))
            .unwrap()
            .node_id()
    }

    /// One announcement wire row (camelCase), as `GET /announcements` returns it.
    #[allow(clippy::too_many_arguments)]
    fn ann_json(
        id: &str,
        package_id: &str,
        own: bool,
        state: &str,
        supersedes: &[&str],
        reject_reason: Option<&str>,
        holders: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "packageId": package_id,
            "publisherDisplayName": "Pub",
            "own": own,
            "rootHash": "a".repeat(64),
            "byteSize": 100,
            "frameCount": 3,
            "aggregateStats": { "manifestXxh3": "abcd1234" },
            "supersedes": supersedes,
            "state": state,
            "rejectReason": reject_reason,
            "createdAt": "2026-07-13T00:00:00Z",
            "decidedAt": null,
            "holders": holders,
        })
    }

    /// A fresh unknown PUBLISHED foreign announcement polls in as a `newPackage`
    /// diff (skipping an own one) and lands a `remote` row.
    #[tokio::test]
    async fn poll_new_published_foreign_is_new_package_skips_own() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                ann_json(
                    "ann-f",
                    "pkg-foreign",
                    false,
                    "published",
                    &[],
                    None,
                    serde_json::json!([])
                ),
                ann_json(
                    "ann-mine",
                    "pkg-mine",
                    true,
                    "published",
                    &[],
                    None,
                    serde_json::json!([])
                ),
            ])))
            .mount(&server)
            .await;

        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());

        let changes = refresh_project_packages(&ctx, "p-1").await.unwrap();
        assert_eq!(
            changes.len(),
            1,
            "only the foreign published package is a newPackage"
        );
        assert_eq!(changes[0].kind, "newPackage");
        assert_eq!(changes[0].package_id, "pkg-foreign");
        assert_eq!(changes[0].project_id, "p-1");

        let conn = db(&ctx).unwrap().conn();
        let foreign = get_package(&conn, "pkg-foreign").unwrap().unwrap();
        assert_eq!(foreign.origin, "remote");
        assert_eq!(foreign.state, "published");
        assert_eq!(foreign.manifest_xxh3.as_deref(), Some("abcd1234"));
        let mine = get_package(&conn, "pkg-mine").unwrap().unwrap();
        assert_eq!(mine.origin, "mine");
        assert!(mine.own);
    }

    /// A fresh unknown PENDING foreign announcement polls in as an
    /// `awaitingApproval` diff — a coordinator's view of someone else's
    /// contribution awaiting a decision — NOT a `newPackage`. A second poll, with
    /// the row now known and still pending, raises nothing.
    #[tokio::test]
    async fn poll_pending_foreign_is_awaiting_approval_then_idempotent() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([ann_json(
                    "ann-p",
                    "pkg-pending",
                    false,
                    "pending",
                    &[],
                    None,
                    serde_json::json!([])
                ),])),
            )
            .mount(&server)
            .await;

        let (_tmp, ctx) = test_ctx();
        wire_hub(&ctx, &server.uri());

        let changes = refresh_project_packages(&ctx, "p-1").await.unwrap();
        assert_eq!(
            changes.len(),
            1,
            "a foreign pending row is one awaitingApproval change"
        );
        assert_eq!(changes[0].kind, "awaitingApproval");
        assert_eq!(changes[0].package_id, "pkg-pending");
        assert_eq!(changes[0].project_id, "p-1");
        assert!(
            changes.iter().all(|c| c.kind != "newPackage"),
            "a pending row is never a newPackage"
        );

        // Second poll: the row is now known and still pending → no diff.
        let again = refresh_project_packages(&ctx, "p-1").await.unwrap();
        assert!(
            again.is_empty(),
            "a known pending row raises nothing on re-poll"
        );
    }

    /// An own package moving `pending → published` across two polls diffs as
    /// `approved` (and the first poll, still pending, raises nothing).
    #[tokio::test]
    async fn poll_own_pending_then_published_is_approved() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let (_tmp, ctx) = test_ctx();

        // Poll 1: own + pending → no diff (not published, and own is skipped).
        let server1 = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([ann_json(
                    "ann-1",
                    "pkg-own",
                    true,
                    "pending",
                    &[],
                    None,
                    serde_json::json!([])
                )])),
            )
            .mount(&server1)
            .await;
        wire_hub(&ctx, &server1.uri());
        assert!(refresh_project_packages(&ctx, "p-1")
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            get_package(&db(&ctx).unwrap().conn(), "pkg-own")
                .unwrap()
                .unwrap()
                .state,
            "pending"
        );

        // Poll 2: the same package is now published → approved.
        let server2 = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([ann_json(
                    "ann-1",
                    "pkg-own",
                    true,
                    "published",
                    &[],
                    None,
                    serde_json::json!([])
                )])),
            )
            .mount(&server2)
            .await;
        wire_hub(&ctx, &server2.uri());
        let changes = refresh_project_packages(&ctx, "p-1").await.unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, "approved");
        assert_eq!(changes[0].package_id, "pkg-own");

        // Idempotent: a third identical poll raises nothing (already published).
        let changes2 = refresh_project_packages(&ctx, "p-1").await.unwrap();
        assert!(
            changes2.is_empty(),
            "re-polling a published row does not re-approve"
        );
    }

    /// A known package moving to `rejected` diffs as `rejected` carrying the hub
    /// reason as `detail`.
    #[tokio::test]
    async fn poll_rejected_carries_reason() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let (_tmp, ctx) = test_ctx();
        // Seed a known pending row directly.
        {
            let conn = db(&ctx).unwrap().conn();
            let mut row = base_package("pkg-r", "p-1", "Pub");
            row.own = true;
            row.origin = "mine".into();
            row.state = "pending".into();
            upsert_package(&conn, &row).unwrap();
        }

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([ann_json(
                    "ann-r",
                    "pkg-r",
                    true,
                    "rejected",
                    &[],
                    Some("FWHM too high"),
                    serde_json::json!([])
                )])),
            )
            .mount(&server)
            .await;
        wire_hub(&ctx, &server.uri());

        let changes = refresh_project_packages(&ctx, "p-1").await.unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, "rejected");
        assert_eq!(changes[0].detail.as_deref(), Some("FWHM too high"));
        let row = get_package(&db(&ctx).unwrap().conn(), "pkg-r")
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "rejected");
        assert_eq!(row.reject_reason.as_deref(), Some("FWHM too high"));
    }

    /// The hub-listed `supersedes` array marks the older package superseded, and —
    /// the T3 hazard — that flag SURVIVES a re-poll even though every upsert
    /// rewrites `superseded=0`. Holder/online counts are captured per poll.
    #[tokio::test]
    async fn poll_marks_supersedes_and_survives_repoll() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let (_tmp, ctx) = test_ctx();
        // A holder seen "now" is online; one seen long ago is not.
        let now = chrono::Utc::now().to_rfc3339();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                // The new package supersedes ann-old.
                ann_json("ann-new", "pkg-new", true, "published", &["ann-old"], None, serde_json::json!([
                    { "pubkey": "cHVia2V5", "displayName": "H1", "lastSeenAt": now },
                    { "pubkey": "cHVia2V5Mg", "displayName": "H2", "lastSeenAt": "2020-01-01T00:00:00Z" },
                ])),
                // The older, still-own package whose announcement is superseded.
                ann_json("ann-old", "pkg-old", true, "published", &[], None, serde_json::json!([])),
            ])))
            .mount(&server)
            .await;
        wire_hub(&ctx, &server.uri());

        refresh_project_packages(&ctx, "p-1").await.unwrap();
        {
            let conn = db(&ctx).unwrap().conn();
            let old = get_package(&conn, "pkg-old").unwrap().unwrap();
            assert!(
                old.superseded,
                "pkg-old is superseded by ann-new's supersedes list"
            );
            let new = get_package(&conn, "pkg-new").unwrap().unwrap();
            assert!(!new.superseded, "pkg-new is not superseded");
            assert_eq!(new.holder_count, 2, "holder_count = holders.len()");
            assert_eq!(
                new.online_count, 1,
                "only the recently-seen holder counts as online"
            );
        }

        // Re-poll: every upsert rewrote superseded=0, but the comprehensive
        // re-mark restores it (T3 hazard).
        refresh_project_packages(&ctx, "p-1").await.unwrap();
        assert!(
            get_package(&db(&ctx).unwrap().conn(), "pkg-old")
                .unwrap()
                .unwrap()
                .superseded,
            "own supersede flag survives a re-poll"
        );
    }

    /// Download role guard (fail-closed): a send-only own node is refused with
    /// `Invalid` before any network I/O.
    #[tokio::test]
    async fn download_role_guard_send_only_is_invalid() {
        let (_tmp, ctx) = test_ctx();
        let own = own_node_for(&ctx);
        {
            let conn = db(&ctx).unwrap().conn();
            let members = serde_json::json!([
                { "accountId": "acc-me", "displayName": "Me", "dataRole": "send",
                  "coordinator": false, "nodes": [B64.encode(own)] }
            ])
            .to_string();
            seed_project(&conn, "p-1", &members);
        }
        let sync = crate::sync::SyncRuntime::new();
        let err = download_project_package(&ctx, &sync, "p-1", "pkg-x", None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ApiError::Invalid(_)),
            "send-only is fail-closed Invalid, got {err:?}"
        );
    }

    /// F3: a download whose holders are exhausted lands `failed` AND buffers a
    /// `downloadFailed` change that the next `refresh_all_project_packages` drains
    /// exactly once — the spawned pull task can't `notify()` the UI itself.
    #[tokio::test]
    async fn exhausted_download_buffers_downloadfailed_drained_once() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Drains the process-global change buffer — serialize against the other
        // draining test so neither steals the other's entries (D3 T5).
        let _drain_guard = drain_lock();
        let (_tmp, ctx) = test_ctx();
        let own = own_node_for(&ctx);
        {
            let conn = db(&ctx).unwrap().conn();
            let members = serde_json::json!([
                { "accountId": "acc-me", "displayName": "Me", "dataRole": "send_receive",
                  "coordinator": false, "nodes": [B64.encode(own)] }
            ])
            .to_string();
            seed_project(&conn, "p-1", &members);
        }

        // The hub lists the package with NO holders ⇒ the pull exhausts and fails.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([ann_json(
                    "ann-x",
                    "pkg-x",
                    false,
                    "published",
                    &[],
                    None,
                    serde_json::json!([])
                )])),
            )
            .mount(&server)
            .await;
        wire_hub(&ctx, &server.uri());

        let sync = crate::sync::SyncRuntime::new();
        // Role passes, poll finds the package, no other holder to pull from ⇒ failed.
        download_project_package(&ctx, &sync, "p-1", "pkg-x", None)
            .await
            .unwrap();
        assert_eq!(
            get_package(&db(&ctx).unwrap().conn(), "pkg-x")
                .unwrap()
                .unwrap()
                .local_status,
            "failed",
            "an exhausted download lands failed"
        );

        // The next refresh drains exactly one downloadFailed for pkg-x. Scoped to
        // THIS package id on purpose: the F3 buffer is a process-global static, so
        // any other test whose download exhausts in the same binary contributes its
        // own entry to the same drain (D3 T3 added one).
        let changes = refresh_all_project_packages(&ctx).await.unwrap();
        let dl_failed: Vec<_> = changes
            .iter()
            .filter(|c| c.kind == "downloadFailed" && c.package_id == "pkg-x")
            .collect();
        assert_eq!(
            dl_failed.len(),
            1,
            "exactly one buffered downloadFailed drained"
        );
        assert_eq!(dl_failed[0].project_id, "p-1");

        // Drained exactly once — a second refresh surfaces no more.
        let again = refresh_all_project_packages(&ctx).await.unwrap();
        assert!(
            again
                .iter()
                .all(|c| !(c.kind == "downloadFailed" && c.package_id == "pkg-x")),
            "the buffer is not re-drained"
        );
    }

    /// F1: two pulls of ONE package must never run at once. The swarm staging
    /// dir, the collection tag and the row's `local_status` are all keyed by the
    /// package id alone, so an overlapping pass — the 20-minute worker re-admits
    /// `downloading` AND `failed` rows, "Sync now" spawns an un-deduped pass, and
    /// Retry calls straight in — would have the faster pull's staging cleanup +
    /// blob release yank the directory and the bytes out from under the slower
    /// one, whose failure then re-arms `downloading` over the winner's `complete`
    /// and finally stamps `failed` on a fully landed, seeded package. The second
    /// call is a logged skip (an in-flight pull IS the requested work), so the hub
    /// sees exactly ONE announcement poll.
    #[tokio::test]
    async fn concurrent_pulls_of_one_package_run_once() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let (_tmp, ctx) = test_ctx();
        let own = own_node_for(&ctx);
        {
            let conn = db(&ctx).unwrap().conn();
            let members = serde_json::json!([
                { "accountId": "acc-me", "displayName": "Me", "dataRole": "send_receive",
                  "coordinator": false, "nodes": [B64.encode(own)] }
            ])
            .to_string();
            seed_project(&conn, "p-1", &members);
        }

        // The hub answer is SLOW on purpose: the first pull is provably still
        // inside it when the second call starts. No holders ⇒ whichever pull gets
        // through exhausts and lands `failed`.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/projects/p-1/announcements"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(400))
                    .set_body_json(serde_json::json!([ann_json(
                        "ann-conc",
                        "pkg-conc",
                        false,
                        "published",
                        &[],
                        None,
                        serde_json::json!([])
                    )])),
            )
            .mount(&server)
            .await;
        wire_hub(&ctx, &server.uri());

        let ctx = Arc::new(ctx);
        let sync = Arc::new(crate::sync::SyncRuntime::new());
        let first = tokio::spawn({
            let ctx = Arc::clone(&ctx);
            let sync = Arc::clone(&sync);
            async move { download_project_package(&ctx, &sync, "p-1", "pkg-conc", None).await }
        });
        // Let the first pull reach the (delayed) hub call before the second starts.
        tokio::time::sleep(Duration::from_millis(80)).await;
        download_project_package(&ctx, &sync, "p-1", "pkg-conc", None)
            .await
            .expect("the concurrent pull is a skip, not an error");
        first.await.unwrap().unwrap();

        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "only one pull polled the hub; the concurrent one was skipped"
        );
    }

    /// F1: a failing attempt must never stamp `failed` over a package another
    /// writer already landed. The row would lie about a fully ingested + seeded
    /// package, the need diff would re-admit it on every pass, and the UI would
    /// offer a Retry for work that is done.
    #[test]
    fn set_download_failed_never_clobbers_a_complete_package() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = db(&ctx).unwrap().conn();
            seed_project(&conn, "p-1", &members_json());
            let mut row = base_package("pkg-done", "p-1", "Alice");
            row.local_status = "complete".to_string();
            upsert_package(&conn, &row).unwrap();
        }

        set_download_failed(&ctx, "p-1", "pkg-done", Some("no holder delivered".into()));

        assert_eq!(
            get_package(&db(&ctx).unwrap().conn(), "pkg-done")
                .unwrap()
                .unwrap()
                .local_status,
            "complete",
            "a landed package keeps its status"
        );
    }

    /// F1: the post-swarm re-arm never resurrects a package another writer
    /// completed while the swarm attempt was in flight (the receiver's push arm
    /// can land the same package); a `failed` / `none` row is re-armed as before,
    /// otherwise the sequential fallback would skip every holder on a stale
    /// verdict.
    #[test]
    fn fallback_rearm_skips_a_completed_row() {
        let conn = test_conn();
        seed_project(&conn, "p-1", &members_json());
        let mut done = base_package("pkg-done", "p-1", "Alice");
        done.local_status = "complete".to_string();
        upsert_package(&conn, &done).unwrap();
        let mut broken = base_package("pkg-broken", "p-1", "Alice");
        broken.local_status = "failed".to_string();
        upsert_package(&conn, &broken).unwrap();

        assert!(
            !rearm_for_fallback(&conn, "pkg-done").unwrap(),
            "complete is left alone"
        );
        assert_eq!(
            get_package(&conn, "pkg-done")
                .unwrap()
                .unwrap()
                .local_status,
            "complete"
        );
        assert!(
            rearm_for_fallback(&conn, "pkg-broken").unwrap(),
            "a failed row re-arms"
        );
        assert_eq!(
            get_package(&conn, "pkg-broken")
                .unwrap()
                .unwrap()
                .local_status,
            "downloading"
        );
    }

    /// Download happy path over loopback: D role-passes, polls the hub for the
    /// package's holder (A), requests the serve, A completes the Task-6 serve
    /// circuit, D's receiver ingests (flipping `local_status` to complete), the
    /// poll-loop observes it, and `report_have` hits the hub.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn download_happy_path_over_loopback() {
        use crate::sharing::loopback::LoopbackNetwork;
        use crate::sync::{
            allow_all_peers, ProjectAnnounceGate, ProjectReceiveHooks, StandaloneSyncStore,
            SyncReceiver, SyncRuntime,
        };
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const PROJECT: &str = "proj-dl";
        const HUB: &str = "hub-dl-1";

        let (dtmp, ctx) = test_ctx();
        let own_node = own_node_for(&ctx);
        let d_dirs = crate::api::sync::sync_dirs(&ctx).unwrap();
        let (d_sync_dir, d_db_path) = (d_dirs.working_dir.clone(), d_dirs.db_path.clone());

        // ── A: a received-complete package + its reconstructed serve dir. ──────
        let a_tmp = tempfile::tempdir().unwrap();
        let payload = b"downloaded-frame-bytes-0001";
        let a_db = crate::db::Database::new(a_tmp.path().join("a_catalog.db")).unwrap();
        let manifest_bytes = {
            let c = a_db.conn();
            seed_received_package(
                &c,
                &a_tmp.path().join("a_land"),
                PROJECT,
                HUB,
                "Alice",
                "published",
                payload,
            )
        };
        // The hub-anchored manifest hash the receiver verifies the served bytes
        // against — must be the REAL anchor of A's manifest, not a placeholder.
        let anchor = hash_bytes(&manifest_bytes);
        let serve_dir = {
            let c = a_db.conn();
            reconstruct_serve_dir(&c, &a_tmp.path().join("a_sync"), HUB).unwrap()
        };

        // ── Loopback endpoints: D (receiver + request sender), A recv + A send. ─
        let net = LoopbackNetwork::new();
        let d_ep: Arc<crate::sharing::loopback::LoopbackTransport> = Arc::new(net.endpoint());
        let d_node = d_ep.node_id();
        let a_recv_ep = net.endpoint();
        let a_recv_node = a_recv_ep.node_id();
        let a_send_ep = net.endpoint();
        let a_send_node = a_send_ep.node_id();

        // A's collab engine serves back to D over a_send.
        let a_store = Arc::new(StandaloneSyncStore::open(a_tmp.path().join("a_sync.db")).unwrap());
        let a_engine = Arc::new(SyncEngine::spawn_with_sink_and_emitter(
            Arc::clone(&a_store) as Arc<dyn SyncStore>,
            Arc::new(a_send_ep) as Arc<dyn SharingTransport>,
            d_node,
            Arc::new(CollabCleanupSink),
            None,
        ));

        // A's receiver: on an inbound request, enqueue the reconstructed serve dir.
        let handler_engine = Arc::clone(&a_engine);
        let handler_dir = serve_dir.clone();
        let request_handler: crate::sync::ProjectRequestHandler =
            Arc::new(move |from: NodeId, _project: String, _package: String| {
                assert_eq!(from, d_node, "the serve request came from D");
                let e = Arc::clone(&handler_engine);
                let dir = handler_dir.clone();
                tokio::spawn(async move {
                    let _ = e
                        .enqueue_package(&dir, None, Vec::new(), PackageLayout::Batch)
                        .await;
                });
            });
        let a_recv_store =
            Arc::new(CatalogSyncStore::open(a_tmp.path().join("a_recv.db")).unwrap());
        let a_incoming: crate::sync::receiver::IncomingResolver = {
            let p = a_tmp.path().join("a_incoming");
            Arc::new(move || p.clone())
        };
        let (_a_info, _a_handle) = SyncReceiver::spawn(
            a_recv_store,
            a_tmp.path().join("a_stage"),
            a_incoming,
            allow_all_peers(),
            ProjectReceiveHooks {
                request_handler: Some(request_handler),
                ..Default::default()
            },
            Arc::new(crate::sync::InboundControl::new()),
            Arc::new(a_recv_ep) as Arc<dyn SharingTransport>,
            Arc::new(crate::events::NullEmitter),
        )
        .await
        .unwrap();

        // ── D: seed the catalog, spawn the receiver, hold the runtime. ─────────
        {
            let conn = db(&ctx).unwrap().conn();
            // Own node is a send_receive member → role guard passes.
            let members = serde_json::json!([
                { "accountId": "acc-me", "displayName": "Me", "dataRole": "send_receive",
                  "coordinator": true, "nodes": [B64.encode(own_node)] }
            ])
            .to_string();
            seed_project(&conn, PROJECT, &members);
            // The hub package row (from a prior poll) — origin remote, not yet held.
            let row = base_package(HUB, PROJECT, "Alice");
            upsert_package(&conn, &row).unwrap();
            // The collaboration landing root project_ingest fills.
            conn.execute(
                "INSERT INTO scan_roots (path, kind) VALUES (?1, 'collaboration')",
                [dtmp.path().join("d_land").to_string_lossy().to_string()],
            )
            .unwrap();
        }

        let d_store = Arc::new(CatalogSyncStore::open(&d_db_path).unwrap());
        // D's project gate authorizes the announce from A's SENDER node.
        let gate: ProjectAnnounceGate =
            Arc::new(move |from: &NodeId, pid: &str| *from == a_send_node && pid == PROJECT);
        let d_incoming: crate::sync::receiver::IncomingResolver = {
            let p = d_sync_dir.join("incoming");
            Arc::new(move || p.clone())
        };
        let (_d_info, d_handle) = SyncReceiver::spawn(
            d_store,
            d_sync_dir.clone(),
            d_incoming,
            allow_all_peers(),
            ProjectReceiveHooks {
                gate: Some(gate),
                ..Default::default()
            },
            Arc::new(crate::sync::InboundControl::new()),
            Arc::clone(&d_ep) as Arc<dyn SharingTransport>,
            Arc::new(crate::events::NullEmitter),
        )
        .await
        .unwrap();

        let runtime = SyncRuntime::new();
        runtime
            .set_started_for_test(
                Arc::clone(&d_ep) as Arc<dyn SharingTransport>,
                d_handle,
                "ticket".into(),
            )
            .await;

        // ── Hub: list the package with A (recv node) as its holder + accept have. ─
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/projects/{PROJECT}/announcements")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "id": "ann-hub",
                    "packageId": HUB,
                    "publisherDisplayName": "Alice",
                    "own": false,
                    "rootHash": "a".repeat(64),
                    "byteSize": payload.len(),
                    "frameCount": 1,
                    // The REAL manifest anchor — the receiver verifies against it.
                    "aggregateStats": { "manifestXxh3": anchor },
                    "supersedes": [],
                    "state": "published",
                    "rejectReason": null,
                    "createdAt": "2026-07-13T00:00:00Z",
                    "decidedAt": null,
                    "holders": [
                        { "pubkey": B64.encode(a_recv_node), "displayName": "Alice",
                          "lastSeenAt": chrono::Utc::now().to_rfc3339() }
                    ],
                }])),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/announcements/ann-hub/have"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        wire_hub(&ctx, &server.uri());

        // ── Drive the download. ────────────────────────────────────────────────
        download_project_package(&ctx, &runtime, PROJECT, HUB, None)
            .await
            .unwrap();

        // D holds the package: local_status complete, one landed contribution
        // (never into files/frames), and the hub was told we now have it.
        let conn = db(&ctx).unwrap().conn();
        assert_eq!(
            get_package(&conn, HUB).unwrap().unwrap().local_status,
            "complete",
            "the download loop observed the ingest"
        );
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM files"),
            0,
            "contributions never enter files"
        );
        let landed = contributions_for_package(&conn, HUB).unwrap();
        assert_eq!(landed.len(), 1, "the served frame landed as a contribution");
        assert_eq!(std::fs::read(&landed[0].landed_path).unwrap(), payload);

        // report_have was asserted via `.expect(1)`; verify explicitly too.
        let haves = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path() == "/api/v1/announcements/ann-hub/have")
            .count();
        assert_eq!(haves, 1, "report_have hit the hub once after ingest");

        a_engine.shutdown().await;

        // D3 T3: the swarm attempt ran FIRST and failed (the loopback transport
        // has no multi-source fetch), so this completion is the sequential
        // fallback's — proof the fallback still delivers with the swarm path in
        // front of it, not proof the swarm block was skipped.
        assert!(
            swarm_unfit_snapshot().contains(HUB),
            "the swarm attempt was made and cached its unfit verdict"
        );
    }

    // ── D3 Task 3: the swarm download path ───────────────────────────────────

    /// One holder tuple in the shape `download_project_package` builds.
    fn holder_tuple(n: NodeId, relay: Option<&str>) -> (NodeId, String, Option<String>) {
        (n, "Holder".to_string(), relay.map(str::to_string))
    }

    /// No holder at all ⇒ nothing to fan out to (the caller's own no-holders
    /// guard already covers this; the plan refuses independently).
    #[test]
    fn swarm_fetch_plan_none_without_holders() {
        assert!(swarm_fetch_plan(&[], NODE_SR, true, &HashSet::new(), "pkg-1").is_none());
    }

    /// I cannot serve myself: a holder list of only this device plans nothing.
    #[test]
    fn swarm_fetch_plan_none_when_only_self_holds() {
        let holders = vec![holder_tuple(NODE_SR, None)];
        assert!(swarm_fetch_plan(&holders, NODE_SR, true, &HashSet::new(), "pkg-1").is_none());
    }

    /// A cached swarm-unfit verdict (spec §3.2 legacy discrimination) refuses
    /// this package only — its neighbours still plan.
    #[test]
    fn swarm_fetch_plan_none_when_package_is_swarm_unfit() {
        let holders = vec![holder_tuple(NODE_COORD, None)];
        let unfit: HashSet<String> = ["pkg-1".to_string()].into_iter().collect();
        assert!(swarm_fetch_plan(&holders, NODE_SR, true, &unfit, "pkg-1").is_none());
        assert!(
            swarm_fetch_plan(&holders, NODE_SR, true, &unfit, "pkg-2").is_some(),
            "another package is unaffected by pkg-1's verdict"
        );
    }

    /// A root hash that cannot even be a collection hash never reaches the wire.
    #[test]
    fn swarm_fetch_plan_none_when_the_root_hash_is_not_swarm_capable() {
        let holders = vec![holder_tuple(NODE_COORD, None)];
        assert!(swarm_fetch_plan(&holders, NODE_SR, false, &HashSet::new(), "pkg-1").is_none());
    }

    /// Shape gate only — 64 hex characters. A legacy manifest identifier passes
    /// it too (by design: the real discrimination is try-then-fallback).
    #[test]
    fn collection_hash_shape_is_64_hex() {
        assert!(looks_like_collection_hash(&"a".repeat(64)));
        assert!(
            looks_like_collection_hash(&"F".repeat(64)),
            "case-insensitive"
        );
        assert!(!looks_like_collection_hash(""));
        assert!(
            !looks_like_collection_hash("rh"),
            "the pre-D3 placeholder value"
        );
        assert!(!looks_like_collection_hash(&"a".repeat(63)));
        assert!(!looks_like_collection_hash(&"a".repeat(65)));
        assert!(!looks_like_collection_hash(&"g".repeat(64)), "not hex");
    }

    /// The plan is every holder but me, each keeping its own relay hint (the
    /// dial-hint builder needs it per provider).
    #[test]
    fn swarm_fetch_plan_lists_every_other_holder_with_its_relay_hint() {
        let holders = vec![
            holder_tuple(NODE_COORD, Some("https://relay.one/")),
            holder_tuple(NODE_SR, None), // self — excluded
            holder_tuple(NODE_SEND_ONLY, None),
        ];
        let plan = swarm_fetch_plan(&holders, NODE_SR, true, &HashSet::new(), "pkg-1").unwrap();
        assert_eq!(
            plan,
            vec![
                (NODE_COORD, Some("https://relay.one/".to_string())),
                (NODE_SEND_ONLY, None),
            ]
        );
    }

    /// The verdict is sticky for the process: once marked, every later plan for
    /// that package refuses, so a second Download in the same session goes
    /// straight to the sequential path without a second multi-fetch attempt.
    #[test]
    fn swarm_unfit_is_cached_for_the_session() {
        const PKG: &str = "pkg-unfit-session";
        let holders = vec![holder_tuple(NODE_COORD, None)];
        assert!(
            swarm_fetch_plan(&holders, NODE_SR, true, &swarm_unfit_snapshot(), PKG).is_some(),
            "the first attempt is planned"
        );
        mark_swarm_unfit(PKG);
        assert!(
            swarm_fetch_plan(&holders, NODE_SR, true, &swarm_unfit_snapshot(), PKG).is_none(),
            "the second attempt is refused for the rest of the session"
        );
    }

    /// F6: the session verdict is scoped to what can never work again this run.
    /// Marking every failure class — which is what the first cut did — blinds the
    /// swarm to the two conditions that change by themselves: holders coming back
    /// online, and a landing fault that rejected a frame.
    #[test]
    fn swarm_unfit_is_cached_only_for_verdicts_that_cannot_change() {
        // No swarm-capable transport: nothing on the network is involved and no
        // retry in this process can do better.
        assert!(cache_swarm_unfit(SwarmStage::FetchMeta, false, false));
        assert!(cache_swarm_unfit(SwarmStage::Ingest, false, false));

        // Dead swarm — nobody served even the collection meta AND the fallback
        // delivered nothing either. Holders come back; the next pass retries
        // cheaply.
        assert!(!cache_swarm_unfit(SwarmStage::FetchMeta, true, false));

        // Same phase-1 failure, but a holder then served the WHOLE package
        // through the fallback: the holders were live, so what the swarm could
        // not resolve is the announced hash (a pre-D3 identifier, or nobody
        // seeding it).
        assert!(cache_swarm_unfit(SwarmStage::FetchMeta, true, true));

        // An ingest rejection is not a statement about the swarm — the fetch
        // worked — whichever way the fallback goes.
        assert!(!cache_swarm_unfit(SwarmStage::Ingest, true, true));
        assert!(!cache_swarm_unfit(SwarmStage::Ingest, true, false));
    }

    /// Wave 1 final review item 1: a phase-2 (child fan-out) failure must never
    /// enter the unfit set, whatever the fallback does — the assignment loop's
    /// stall ceiling and backoff ladder can fail this phase on a condition that
    /// changes by itself, and phase 1 already proved the hash resolves. A
    /// phase-1 (meta) failure keeps the old, unchanged rule: cacheable only when
    /// the fallback proves the holders were reachable.
    #[test]
    fn cache_swarm_unfit_never_caches_a_phase2_failure_but_still_caches_phase1() {
        assert!(
            !cache_swarm_unfit(SwarmStage::FetchChildren, true, true),
            "a phase-2 failure is never cacheable, even when the fallback delivered"
        );
        assert!(
            !cache_swarm_unfit(SwarmStage::FetchChildren, true, false),
            "a phase-2 failure is never cacheable"
        );
        assert!(
            cache_swarm_unfit(SwarmStage::FetchMeta, true, true),
            "a phase-1 failure is still cacheable once the fallback proves the holders were live"
        );
        assert!(
            !cache_swarm_unfit(SwarmStage::FetchMeta, true, false),
            "a phase-1 failure with no fallback delivery stays retryable"
        );
    }

    /// D3 §3.2 legacy discrimination, end to end: an announcement whose
    /// `root_hash` is a well-formed 64-hex value NO provider serves (exactly a
    /// pre-D3 manifest identifier) fails the swarm attempt, caches the
    /// swarm-unfit verdict, and falls through to the sequential holder loop IN
    /// THE SAME CALL — here the loop exhausts (its one holder is not on the
    /// loopback network), which is what proves it was entered.
    #[cfg(unix)]
    #[tokio::test]
    async fn swarm_download_falls_back_when_providers_lack_the_hash() {
        use crate::sharing::loopback::LoopbackNetwork;
        use crate::sync::{allow_all_peers, ProjectReceiveHooks, SyncReceiver, SyncRuntime};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const PROJECT: &str = "proj-swarm-fb";
        const HUB: &str = "hub-swarm-fb-1";

        let (tmp, ctx) = test_ctx();
        let own = own_node_for(&ctx);
        let sync_dir = crate::api::sync::sync_dirs(&ctx).unwrap().working_dir;
        {
            let conn = db(&ctx).unwrap().conn();
            let members = serde_json::json!([
                { "accountId": "acc-me", "displayName": "Me", "dataRole": "send_receive",
                  "coordinator": true, "nodes": [B64.encode(own)] }
            ])
            .to_string();
            seed_project(&conn, PROJECT, &members);
            upsert_package(&conn, &base_package(HUB, PROJECT, "Alice")).unwrap();
        }

        // A started runtime over a loopback endpoint: its `fetch_collection_multi`
        // is the trait's default (bails), which is exactly how a transport without
        // swarm support behaves — the swarm attempt fails at the transport.
        let net = LoopbackNetwork::new();
        let ep = Arc::new(net.endpoint());
        let store = Arc::new(CatalogSyncStore::open(tmp.path().join("recv.db")).unwrap());
        let incoming: crate::sync::receiver::IncomingResolver = {
            let p = sync_dir.join("incoming");
            Arc::new(move || p.clone())
        };
        let (_info, handle) = SyncReceiver::spawn(
            store,
            sync_dir.clone(),
            incoming,
            allow_all_peers(),
            ProjectReceiveHooks::default(),
            Arc::new(crate::sync::InboundControl::new()),
            Arc::clone(&ep) as Arc<dyn SharingTransport>,
            Arc::new(crate::events::NullEmitter),
        )
        .await
        .unwrap();
        let runtime = SyncRuntime::new();
        runtime
            .set_started_for_test(
                Arc::clone(&ep) as Arc<dyn SharingTransport>,
                handle,
                "t".into(),
            )
            .await;

        // The hub lists ONE holder that is not on the loopback network at all, so
        // the sequential fallback's `request_project` cannot route to it.
        let ghost = [0x5A; 32];
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/projects/{PROJECT}/announcements")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([ann_json(
                    "ann-fb",
                    HUB,
                    false,
                    "published",
                    &[],
                    None,
                    serde_json::json!([
                        { "pubkey": B64.encode(ghost), "displayName": "Ghost",
                          "lastSeenAt": chrono::Utc::now().to_rfc3339() }
                    ])
                )])),
            )
            .mount(&server)
            .await;
        wire_hub(&ctx, &server.uri());

        download_project_package(&ctx, &runtime, PROJECT, HUB, None)
            .await
            .unwrap();

        assert!(
            swarm_unfit_snapshot().contains(HUB),
            "the failed swarm attempt cached a swarm-unfit verdict"
        );
        assert_eq!(
            get_package(&db(&ctx).unwrap().conn(), HUB)
                .unwrap()
                .unwrap()
                .local_status,
            "failed",
            "the call fell through to the sequential loop and exhausted it"
        );
        // No second attempt is planned for the rest of the session.
        assert!(
            swarm_fetch_plan(
                &[holder_tuple(ghost, None)],
                own,
                true,
                &swarm_unfit_snapshot(),
                HUB
            )
            .is_none(),
            "the cached verdict sends the next Download straight to the fallback"
        );
    }

    // ── D3 T4: every downloader becomes a seed ───────────────────────────────

    /// Bind a real (relay-disabled) shared node at `ctx`'s sync dir and install it
    /// on the context — exactly where `ensure_iroh_node` leaves it in production,
    /// which is where [`seed_ingested_package`] reads it from.
    async fn bind_node_into(
        ctx: &ServiceContext,
    ) -> Arc<crate::sharing::iroh::node::SharedIrohNode> {
        let dirs = crate::api::sync::sync_dirs(ctx).unwrap();
        std::fs::create_dir_all(&dirs.identity_dir).unwrap();
        std::fs::create_dir_all(&dirs.working_dir).unwrap();
        let node = crate::sharing::iroh::node::SharedIrohNode::bind_with(
            &dirs.identity_dir,
            &dirs.working_dir,
            iroh::RelayMode::Disabled,
            crate::sharing::iroh::node::NodeOptions::default(),
        )
        .await
        .expect("bind relay-disabled node");
        *ctx.iroh_node.lock().await = Some(Arc::clone(&node));
        node
    }

    /// The root hash a node currently seeds for `(project, package)`, or `None`.
    async fn seeded_hash(
        node: &crate::sharing::iroh::node::SharedIrohNode,
        project_id: &str,
        package_id: &str,
    ) -> Option<iroh_blobs::Hash> {
        node.store()
            .tags()
            .get(format!("project/{project_id}/{package_id}").as_bytes())
            .await
            .expect("tags().get")
            .map(|t| t.hash)
    }

    /// Export every child of the collection `root` out of `node`'s store into
    /// `dest` — the local twin of what an incoming GET reads. A blob imported
    /// with `TryReference` is a REFERENCE TO A PATH, so this fails the moment
    /// that path is gone: it is the oracle for "the seed is still servable".
    async fn export_collection_locally(
        node: &crate::sharing::iroh::node::SharedIrohNode,
        root: iroh_blobs::Hash,
        dest: &Path,
    ) -> Result<()> {
        let store = node.store();
        let collection = iroh_blobs::format::collection::Collection::load(root, store)
            .await
            .with_context(|| format!("load collection {root}"))?;
        std::fs::create_dir_all(dest)?;
        for (name, hash) in collection.iter() {
            let target = dest.join(name);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            store
                .blobs()
                .export(*hash, &target)
                .await
                .with_context(|| format!("export {name} ({hash})"))?;
        }
        Ok(())
    }

    /// THE seam (D3 T4). `CollabCleanupSink::on_terminal` deletes
    /// `collab_serve/<pkg>` at every push-serve terminal, and a `TryReference`
    /// blob is a reference to a PATH (vendored `store/fs/import.rs`:
    /// `ImportSource::External(path, …)`) — so seeding the SERVE dir would put
    /// every seeded blob behind a path the next serve terminal deletes, turning
    /// this device into a phantom holder the hub still advertises. The seed
    /// therefore targets its own `collab_seed/<pkg>` tree of hard links, which no
    /// cleanup path touches. Here: seed, then run the real cleanup sink over the
    /// real reconstructed serve dir, then assert the seeded collection still
    /// exports byte-identical content.
    #[cfg(unix)]
    #[tokio::test]
    async fn downloader_seed_survives_collab_serve_cleanup() {
        const PROJECT: &str = "p-seam";
        const HUB: &str = "hub-seam";

        let (tmp, ctx) = test_ctx();
        // 128 KiB: comfortably above the store's inline threshold, so the blob is
        // genuinely an external reference and not data copied into redb — an
        // inlined payload would survive any deletion and prove nothing.
        let payload = vec![0x5Au8; 128 * 1024];
        let landing = tmp.path().join("land");
        {
            let conn = db(&ctx).unwrap().conn();
            seed_received_package(
                &conn,
                &landing,
                PROJECT,
                HUB,
                "Alice",
                "published",
                &payload,
            );
        }
        let node = bind_node_into(&ctx).await;

        seed_ingested_package(&ctx, HUB).await;
        let root = seeded_hash(&node, PROJECT, HUB)
            .await
            .expect("the post-ingest hook seeds the package");

        // The seed target is NOT the serve dir.
        let sync_dir = crate::api::sync::sync_dirs(&ctx).unwrap().working_dir;
        let seed_dir = sync_dir.join(SEED_DIR).join(HUB);
        assert!(
            seed_dir.join("L_0001.fits").exists(),
            "the seed dir holds the payload"
        );

        // Now do what a push-serve does: reconstruct the serve dir and let the
        // real cleanup sink take it at terminal.
        let serve_dir = {
            let conn = db(&ctx).unwrap().conn();
            reconstruct_serve_dir(&conn, &sync_dir, HUB).unwrap()
        };
        assert_ne!(
            serve_dir, seed_dir,
            "the serve dir and the seed dir are separate trees"
        );
        CollabCleanupSink.on_terminal(&serve_dir);
        assert!(
            !serve_dir.exists(),
            "the serve dir is gone (the cleanup really ran)"
        );

        // The seed is untouched and still servable.
        let out = tmp.path().join("exported");
        export_collection_locally(&node, root, &out)
            .await
            .expect("the seeded collection must still export after the serve dir is cleaned");
        assert_eq!(
            std::fs::read(out.join("L_0001.fits")).unwrap(),
            payload,
            "the seed serves byte-identical content"
        );

        node.shutdown().await;
    }

    /// F2: seeding is gated on the package being PUBLISHED, not merely locally
    /// complete. A coordinator's PENDING review copy that is seeded is served
    /// straight out of the blob store to anyone past the connect gate, with none
    /// of `authorize_and_reconstruct_serve`'s pending ⇒ coordinator-only check —
    /// which is exactly the rule spec §6 claims still governs who may pull. The
    /// copy becomes servable the moment the coordinator approves it, through the
    /// same seed path `decide_announcement` calls.
    #[cfg(unix)]
    #[tokio::test]
    async fn pending_review_copy_is_seeded_only_after_approval() {
        const PROJECT: &str = "p-pending-seed";
        const HUB: &str = "hub-pending-seed";

        let (tmp, ctx) = test_ctx();
        let payload = vec![0x11u8; 128 * 1024];
        let landing = tmp.path().join("land");
        {
            let conn = db(&ctx).unwrap().conn();
            seed_received_package(&conn, &landing, PROJECT, HUB, "Alice", "pending", &payload);
        }
        let node = bind_node_into(&ctx).await;

        // The post-ingest hook fires for a review copy too — and must not seed it.
        seed_ingested_package(&ctx, HUB).await;
        assert!(
            seeded_hash(&node, PROJECT, HUB).await.is_none(),
            "a pending review copy is not seeded"
        );

        // The coordinator approves: the hub state lands on the row and the
        // approval's own seed hook runs.
        {
            let conn = db(&ctx).unwrap().conn();
            conn.execute(
                "UPDATE project_packages SET state = 'published' WHERE package_id = ?1",
                [HUB],
            )
            .unwrap();
        }
        seed_approved_announcement(&ctx, &format!("ann-{HUB}")).await;
        assert!(
            seeded_hash(&node, PROJECT, HUB).await.is_some(),
            "approving the announcement seeds the package"
        );

        node.shutdown().await;
    }

    /// A stamped N-frame package dir (manifest + payloads), plus its manifest
    /// anchor and byte size — built exactly the way a publisher builds one.
    ///
    /// MANY children on purpose: `SplitStrategy::Split` issues one request per
    /// collection child and re-shuffles the provider list for each, so a
    /// dozen-plus payloads is what makes "both holders served" a fact rather than
    /// a coin flip.
    fn build_stamped_package(
        src_root: &Path,
        project_id: &str,
        hub_package_id: &str,
        files: usize,
        size: usize,
    ) -> (PathBuf, String, u64) {
        std::fs::create_dir_all(src_root).unwrap();
        let mut records = Vec::with_capacity(files);
        for i in 0..files {
            let name = format!("L_{i:02}.fits");
            let payload_path = src_root.join(&name);
            // Distinct, non-repeating content per file, so a mis-attributed child
            // fails the content check rather than merely a size check.
            let bytes: Vec<u8> = (0..size).map(|j| ((j + i * 97) % 251) as u8).collect();
            std::fs::write(&payload_path, &bytes).unwrap();
            records.push((
                payload_path,
                ManifestRecord {
                    v: MANIFEST_VERSION,
                    frame_uuid: format!("uuid-crown-{i:02}"),
                    origin_catalog_uuid: "cat".to_string(),
                    origin_device: "de".repeat(32),
                    payload_kind: PayloadKind::CalibratedLight,
                    rel_path: name,
                    byte_size: size as u64,
                    xxh3: hash_bytes(&bytes),
                    frame_meta: serde_json::json!({ "object": "M42" }),
                    analysis: None,
                    app_version: "test".to_string(),
                    project: Some(ProjectStamp {
                        project_id: project_id.to_string(),
                        package_id: hub_package_id.to_string(),
                        thresholds_version: None,
                        cal_engine_version: None,
                    }),
                },
            ));
        }
        let pkg_dir = src_root
            .parent()
            .unwrap()
            .join(format!("pkg-{hub_package_id}"));
        let announce = crate::package::write_package(&pkg_dir, records).unwrap();
        let anchor =
            crate::package::xxh3_full_file(&pkg_dir.join(crate::package::MANIFEST_FILENAME))
                .unwrap();
        (pkg_dir, anchor, announce.byte_size)
    }

    /// Total bytes a node's endpoint has sent since bind (relay + direct) — the
    /// per-provider oracle, taken from iroh's own socket counters. The Split
    /// progress stream is lossy upstream (T1's re-gate), so telemetry can never
    /// answer "did this provider serve payload"; this can.
    fn sent_bytes(node: &Arc<crate::sharing::iroh::node::SharedIrohNode>) -> u64 {
        let c = node.counters_snapshot_for_test();
        c.send_direct_bytes.saturating_add(c.send_relay_bytes)
    }

    /// A provider's send delta must clear this to count as "served real payload".
    /// Same floor and same reasoning as the T1 swarm tests: measured non-serving
    /// providers still send ~16 KB of handshake/ACK traffic, one served 96 KiB
    /// child is an order of magnitude above that.
    const SERVED_PAYLOAD_FLOOR: u64 = 64 * 1024;

    /// **The crown e2e** (real QUIC, three nodes): A publishes+seeds (T2), B swarm
    /// -fetches from [A], ingests for real and seeds through the T4 hook, then C
    /// swarm-fetches from [A, B] and BOTH serve payload bytes.
    ///
    /// This is the whole point of D3: the swarm only grows if a downloader turns
    /// into a provider. Two things make that work and both are asserted — B's
    /// seed carries the SAME collection hash A announced (content addressing
    /// across devices: B's seed dir is the retained manifest bytes plus the
    /// landed payloads at their manifest `rel_path`s, so it imports to A's hash),
    /// and after the production `release` of the fetched collection the project
    /// seed is the ONLY tag pinning those blobs on B.
    ///
    /// Scope, honestly: the byte counters prove BOTH holders served payload for
    /// C's fetch. They cannot prove B served *out of its seed dir* — a device
    /// that just fetched a package already holds those blobs as store-owned
    /// copies, and the seed's re-import does not displace them (measured: B's
    /// `blobs/data` still holds one package's worth after seeding). The seed dir
    /// is what the seed references when the store does NOT already hold the
    /// content, and `downloader_seed_survives_collab_serve_cleanup` is the test
    /// that pins that case.
    #[cfg(unix)]
    #[tokio::test]
    async fn swarm_downloader_becomes_a_seed_and_serves_the_next_downloader() {
        use crate::sharing::iroh::node::SharedIrohNode;

        const PROJECT: &str = "proj-crown";
        const HUB: &str = "hub-crown";
        const FILES: usize = 14;
        const SIZE: usize = 96 * 1024;

        // ── A: the publisher, seed №1 (the T2 path) ──────────────────────────
        let a_tmp = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let (pkg_dir, anchor, byte_size) =
            build_stamped_package(&src.path().join("src"), PROJECT, HUB, FILES, SIZE);
        let a_node = SharedIrohNode::bind(&a_tmp.path().join("sync"), iroh::RelayMode::Disabled)
            .await
            .unwrap();
        let a_collab = a_node.handle(Role::Collab);
        let a_info = a_collab.start().await.unwrap();
        let root = a_node
            .seed_project_collection(PROJECT, HUB, &pkg_dir)
            .await
            .expect("A seeds what it published");

        // ── B: a real downloader — fetch, ingest, seed through the T4 hook ────
        let (b_tmp, b_ctx) = test_ctx();
        let b_dirs = crate::api::sync::sync_dirs(&b_ctx).unwrap();
        let (b_sync_dir, b_db_path) = (b_dirs.working_dir.clone(), b_dirs.db_path.clone());
        {
            let conn = db(&b_ctx).unwrap().conn();
            seed_project(&conn, PROJECT, &members_json());
            let mut row = base_package(HUB, PROJECT, "Alice");
            row.origin = "received".to_string();
            row.manifest_xxh3 = Some(anchor.clone());
            row.byte_size = byte_size as i64;
            row.frame_count = FILES as i64;
            row.root_hash = root.to_hex().to_string();
            upsert_package(&conn, &row).unwrap();
        }
        let b_node = bind_node_into(&b_ctx).await;
        let b_collab = b_node.handle(Role::Collab);
        let b_info = b_collab.start().await.unwrap();

        // Relay-disabled endpoints have no discovery: every pair needs the other's
        // address out of band (in production those are the holder dial hints).
        b_node.add_peer_ticket(&a_info.pairing_ticket).unwrap();
        a_node.add_peer_ticket(&b_info.pairing_ticket).unwrap();

        let telemetry: ProviderTelemetrySink = Arc::new(|_| {});
        let staging = b_sync_dir.join(SWARM_STAGING_DIR).join(HUB);
        b_collab
            .fetch_collection_multi(
                vec![a_info.node_id],
                &root.to_string(),
                byte_size,
                &staging,
                noop_fetch_sink(),
                telemetry,
            )
            .await
            .expect("B fetches the package from A");

        // Real ingest over B's own catalog connection, exactly as the swarm path
        // runs it, then the production staging + blob cleanup.
        let outcome = {
            let store = CatalogSyncStore::open(&b_db_path).unwrap();
            crate::sync::ingest_project_package(
                crate::sync::IngestConn::Shared(&store),
                &staging,
                PROJECT,
                HUB,
                "swarm",
            )
            .expect("B ingests the fetched package")
        };
        assert!(
            outcome.failed.is_empty(),
            "every frame ingested: {outcome:?}"
        );
        remove_swarm_staging(&staging);
        b_collab
            .release(&PackageId(root.to_hex().to_string()))
            .await
            .expect("B releases the fetched collection, as the swarm path does");

        seed_ingested_package(&b_ctx, HUB).await;

        assert_eq!(
            seeded_hash(&b_node, PROJECT, HUB).await,
            Some(root),
            "B's seed must BE A's collection — identical bytes, identical hash, or \
             there is no swarm for C to fetch from"
        );
        assert!(
            !tag_present_on(&b_node, &format!("collab/pkg/{}", root.to_hex())).await,
            "after release, the project seed is the only tag pinning these blobs on B"
        );

        // ── C: the second downloader — fetches from [A, B] ───────────────────
        let c_tmp = tempfile::tempdir().unwrap();
        let c_node = SharedIrohNode::bind(&c_tmp.path().join("sync"), iroh::RelayMode::Disabled)
            .await
            .unwrap();
        let c_collab = c_node.handle(Role::Collab);
        let c_info = c_collab.start().await.unwrap();
        for ticket in [&a_info.pairing_ticket, &b_info.pairing_ticket] {
            c_node.add_peer_ticket(ticket).unwrap();
        }
        a_node.add_peer_ticket(&c_info.pairing_ticket).unwrap();
        b_node.add_peer_ticket(&c_info.pairing_ticket).unwrap();

        let a_before = sent_bytes(&a_node);
        let b_before = sent_bytes(&b_node);
        let c_dest = c_tmp.path().join("landed");
        let sink: ProviderTelemetrySink = Arc::new(|_| {});
        c_collab
            .fetch_collection_multi(
                vec![a_info.node_id, b_info.node_id],
                &root.to_string(),
                byte_size,
                &c_dest,
                noop_fetch_sink(),
                sink,
            )
            .await
            .expect("C completes from the two-holder swarm");

        for i in 0..FILES {
            let name = format!("L_{i:02}.fits");
            assert_eq!(
                std::fs::read(pkg_dir.join(&name)).unwrap(),
                std::fs::read(c_dest.join(&name)).unwrap(),
                "{name} must land byte-identical at C"
            );
        }

        let a_sent = sent_bytes(&a_node).saturating_sub(a_before);
        let b_sent = sent_bytes(&b_node).saturating_sub(b_before);
        assert!(
            a_sent > SERVED_PAYLOAD_FLOOR && b_sent > SERVED_PAYLOAD_FLOOR,
            "BOTH holders must have served payload — the downloader-turned-seed is \
             the whole point of D3 (a sent {a_sent} B, b sent {b_sent} B, floor \
             {SERVED_PAYLOAD_FLOOR} B)"
        );

        drop(b_tmp);
        a_node.shutdown().await;
        b_node.shutdown().await;
        c_node.shutdown().await;
    }

    async fn tag_present_on(node: &crate::sharing::iroh::node::SharedIrohNode, name: &str) -> bool {
        node.store()
            .tags()
            .get(name.as_bytes())
            .await
            .expect("tags().get")
            .is_some()
    }

    /// The pass's role gate: `coordinator || data_role == "send_receive"`.
    #[test]
    fn role_allows_replication_matches_the_download_guard() {
        assert!(role_allows_replication("send_receive", false));
        assert!(
            role_allows_replication("send", true),
            "a coordinator may always pull"
        );
        assert!(!role_allows_replication("send", false));
    }

    // ── D3 Task 5: the pending package-change buffer (legacy, Task 12) ─────────

    /// [`PENDING_PACKAGE_CHANGES`] is a process-global static: tests that DRAIN
    /// it (directly or through [`refresh_all_project_packages`]) must not run
    /// concurrently, or they steal each other's entries. Push-only tests need no
    /// lock — a foreign push is filtered out by package id.
    static PKG_CHANGE_DRAIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn drain_lock() -> std::sync::MutexGuard<'static, ()> {
        PKG_CHANGE_DRAIN_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// F7: the buffer is bounded and drop-oldest. Only a UI refresh drains it, so
    /// a headless host or an unopened window lets every worker pass append
    /// forever; past the cap the oldest entries go, keeping the ones a user might
    /// still act on.
    #[test]
    fn buffered_package_changes_are_capped_drop_oldest() {
        let _drain_guard = drain_lock();
        // Start from a known-empty buffer: this static is process-global.
        let known: HashSet<String> = ["p-cap".to_string()].into_iter().collect();
        drain_pending_package_changes(&known);

        for i in 0..(MAX_PENDING_PACKAGE_CHANGES + 50) {
            push_package_changes(vec![PackageStateChange {
                project_id: "p-cap".to_string(),
                package_id: format!("pkg-{i:04}"),
                kind: "newPackage".to_string(),
                detail: None,
            }]);
        }

        let drained = drain_pending_package_changes(&known);
        assert_eq!(
            drained.len(),
            MAX_PENDING_PACKAGE_CHANGES,
            "the buffer is capped"
        );
        assert_eq!(
            drained.first().unwrap().package_id,
            format!("pkg-{:04}", 50),
            "the OLDEST entries are the ones dropped"
        );
        assert_eq!(
            drained.last().unwrap().package_id,
            format!("pkg-{:04}", MAX_PENDING_PACKAGE_CHANGES + 49),
            "the newest entry survives"
        );
    }

    /// F7: a change buffered for a project this device no longer has is dropped at
    /// the drain, not replayed — a notification about a project the user cannot
    /// open, which would otherwise sit in the buffer for the life of the process.
    #[tokio::test]
    async fn buffered_changes_for_a_pruned_project_are_not_replayed() {
        let _drain_guard = drain_lock();
        let (_tmp, ctx) = test_ctx();
        {
            let conn = db(&ctx).unwrap().conn();
            seed_project(&conn, "p-live", &members_json());
        }
        // Clear anything a sibling test left behind, then buffer one change for a
        // live project and one for a project that is gone locally.
        drain_pending_package_changes(&HashSet::new());
        push_download_failure("p-live", "pkg-live", None);
        push_download_failure("p-gone", "pkg-gone", None);

        let changes = refresh_all_project_packages(&ctx).await.unwrap();
        assert!(
            changes.iter().any(|c| c.package_id == "pkg-live"),
            "the live project's change still reaches the UI"
        );
        assert!(
            !changes.iter().any(|c| c.package_id == "pkg-gone"),
            "a pruned project's buffered change is dropped"
        );
        // …and it is not lingering for the next refresh either.
        let again = refresh_all_project_packages(&ctx).await.unwrap();
        assert!(again.iter().all(|c| c.package_id != "pkg-gone"));
    }

    /// F4: only a refresh that actually MOVED a project's package rows kicks the
    /// worker. Kicking on every poll would put the worker on the poll cadence
    /// instead of the 20-minute interval it is designed around.
    #[test]
    fn only_a_real_package_set_change_kicks_the_worker() {
        assert!(
            !kick_auto_sync_if_changed(&[]),
            "a no-op refresh kicks nobody"
        );
        assert!(
            kick_auto_sync_if_changed(&[PackageStateChange {
                project_id: "p-1".into(),
                package_id: "pkg-1".into(),
                kind: "approved".into(),
                detail: None,
            }]),
            "an approval kicks the worker"
        );
    }

    /// F4: the kick actually WAKES the loop — spec §3.3 promises a pass
    /// "immediately after a hub poll that changed any project's package set", and
    /// without it a fresh approval waits out the 20-minute cadence (which reads as
    /// broken to anyone smoke-testing two devices). The interval here is an hour,
    /// so only a kick can produce the second pass.
    #[tokio::test]
    async fn a_package_set_change_wakes_the_auto_sync_loop() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let passes = Arc::new(AtomicUsize::new(0));
        let task = tokio::spawn({
            let passes = Arc::clone(&passes);
            async move {
                auto_sync_loop_inner(
                    Duration::ZERO,
                    Duration::from_secs(3600),
                    auto_sync_kick(),
                    move |_kind| {
                        let passes = Arc::clone(&passes);
                        async move {
                            passes.fetch_add(1, Ordering::SeqCst);
                        }
                    },
                )
                .await
            }
        });

        wait_until(
            || passes.load(Ordering::SeqCst) >= 1,
            Duration::from_secs(5),
        )
        .await;
        auto_sync_kick().notify_one();
        wait_until(
            || passes.load(Ordering::SeqCst) >= 2,
            Duration::from_secs(5),
        )
        .await;
        task.abort();
    }

    /// The api-level toggle persists through the db accessor pair.
    #[test]
    fn set_project_auto_replicate_persists() {
        let (_tmp, ctx) = test_ctx();
        {
            let conn = db(&ctx).unwrap().conn();
            seed_project(&conn, "p-auto", &members_json());
        }

        let toggle = |ctx: &ServiceContext| {
            let conn = db(ctx).unwrap().conn();
            crate::db::collab::get_project(&conn, "p-auto")
                .unwrap()
                .unwrap()
                .auto_replicate
        };

        set_project_auto_replicate(&ctx, "p-auto", false).unwrap();
        assert!(!toggle(&ctx));

        set_project_auto_replicate(&ctx, "p-auto", true).unwrap();
        assert!(toggle(&ctx));

        assert!(
            set_project_auto_replicate(&ctx, "no-such-project", false).is_err(),
            "an unknown project is a user-visible error, not a silent no-op"
        );
    }

    // ── Task 8 (wave 2): version poll + manifest delta, against the fake hub ──

    /// Shared fixture for the `poll` / `manifest` tests: one app context signed
    /// in to ONE stateful fake hub as `acc-me`, plus a second member `acc-o`.
    #[cfg(all(feature = "render", feature = "solver"))]
    mod v3_fx {
        use super::*;
        pub(super) use crate::collab::fake_hub::FakeHub;
        pub(super) use crate::collab::hub_client::FrameViewWire;
        pub(super) use crate::db::collab_frames::{self as frames_db, FrameOrigin, LocalFrameRow};

        pub(super) const PID: &str = "p1";

        pub(super) struct Fx {
            pub tmp: tempfile::TempDir,
            pub ctx: ServiceContext,
            pub hub: FakeHub,
        }

        /// `acc-me` (this context, token `tok`) is a `role` member; `acc-o` a
        /// `send_receive` member. The project starts at version 1 with the
        /// fake's default dictionary (version 1) and no thresholds.
        pub(super) async fn fx(role: &str, coordinator: bool, require_approval: bool) -> Fx {
            let hub = FakeHub::start().await;
            let (tmp, ctx) = test_ctx();
            wire_hub(&ctx, &hub.uri());
            hub.add_account("tok", "acc-me", "Me", &B64.encode(own_node_for(&ctx)), None);
            hub.add_account("tok-o", "acc-o", "Other", &B64.encode([0x55u8; 32]), None);
            hub.add_project(
                PID,
                "m31",
                &[
                    ("acc-me", role, coordinator),
                    ("acc-o", "send_receive", false),
                ],
                require_approval,
            );
            Fx { tmp, ctx, hub }
        }

        pub(super) fn rows(ctx: &ServiceContext) -> Vec<LocalFrameRow> {
            frames_db::list_for_project(&db(ctx).unwrap().conn(), PID).unwrap()
        }

        pub(super) fn row(ctx: &ServiceContext, uuid: &str) -> Option<LocalFrameRow> {
            frames_db::get(&db(ctx).unwrap().conn(), PID, uuid).unwrap()
        }

        pub(super) fn project(ctx: &ServiceContext) -> Option<CollabProjectRow> {
            crate::db::collab::get_project(&db(ctx).unwrap().conn(), PID).unwrap()
        }

        pub(super) async fn request_count(hub: &FakeHub) -> usize {
            hub.server.received_requests().await.unwrap().len()
        }

        /// `(method, path)` of every request after the first `from`.
        pub(super) async fn requests_since(hub: &FakeHub, from: usize) -> Vec<(String, String)> {
            hub.server.received_requests().await.unwrap()[from..]
                .iter()
                .map(|r| (r.method.to_string(), r.url.path().to_string()))
                .collect()
        }

        /// `(since, after)` of every manifest request after the first `from`.
        pub(super) async fn manifest_queries(
            hub: &FakeHub,
            from: usize,
        ) -> Vec<(i64, Option<String>)> {
            hub.server.received_requests().await.unwrap()[from..]
                .iter()
                .filter(|r| r.url.path().ends_with("/manifest"))
                .map(|r| {
                    let q = |k: &str| {
                        r.url
                            .query_pairs()
                            .find(|(key, _)| key == k)
                            .map(|(_, v)| v.into_owned())
                    };
                    (q("since").unwrap().parse().unwrap(), q("after"))
                })
                .collect()
        }

        /// Records every emitted event.
        #[derive(Default)]
        pub(super) struct RecordingEmitter {
            pub events: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
        }

        impl ProgressEmitter for RecordingEmitter {
            fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
                self.events
                    .lock()
                    .unwrap()
                    .push((event_name.to_string(), payload));
            }
        }

        impl RecordingEmitter {
            /// `(kind, count)` of every `collab-frames-changed` event, sorted.
            pub(super) fn frame_changes(&self) -> Vec<(String, u64)> {
                let mut out: Vec<(String, u64)> = self
                    .events
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(name, _)| name == "collab-frames-changed")
                    .map(|(_, p)| {
                        assert_eq!(p["projectId"], PID);
                        (
                            p["kind"].as_str().unwrap().to_string(),
                            p["count"].as_u64().unwrap(),
                        )
                    })
                    .collect();
                out.sort();
                out
            }
        }

        /// A real relay-disabled node installed on `ctx`, with the
        /// Collaboration root `<tmp>/Collab` mounted. Returns the node and the
        /// stored root.
        pub(super) async fn bind_node_with_collab_root(
            fx: &Fx,
        ) -> (Arc<crate::sharing::iroh::node::SharedIrohNode>, PathBuf) {
            let dirs = crate::api::sync::sync_dirs(&fx.ctx).unwrap();
            std::fs::create_dir_all(&dirs.identity_dir).unwrap();
            std::fs::create_dir_all(&dirs.working_dir).unwrap();
            let node = crate::sharing::iroh::node::SharedIrohNode::bind_with(
                &dirs.identity_dir,
                &dirs.working_dir,
                iroh::RelayMode::Disabled,
                crate::sharing::iroh::node::NodeOptions::default(),
            )
            .await
            .expect("bind relay-disabled node");
            *fx.ctx.iroh_node.lock().await = Some(Arc::clone(&node));
            let requested = fx.tmp.path().join("Collab");
            std::fs::create_dir_all(&requested).unwrap();
            let collab = PathBuf::from(
                crate::api::scan_roots::set_collaboration_dir(
                    &fx.ctx,
                    requested.to_string_lossy().to_string(),
                    &crate::api::PathPolicy::AllowAll,
                )
                .await
                .unwrap(),
            );
            assert!(node.collab_store().is_some(), "the collab store is mounted");
            (node, collab)
        }

        /// Tags under `<prefix>` in the collab store.
        pub(super) async fn collab_tags(
            node: &crate::sharing::iroh::node::SharedIrohNode,
            prefix: &str,
        ) -> usize {
            use n0_future::StreamExt as _;
            let store = node.collab_store().expect("collab store mounted");
            let mut stream = store.tags().list_prefix(prefix.as_bytes()).await.unwrap();
            let mut n = 0;
            while let Some(item) = stream.next().await {
                item.unwrap();
                n += 1;
            }
            n
        }

        /// `(level, message)` of every event on THIS thread while the capture
        /// lives (a `#[tokio::test]` runs its body on one thread) — the
        /// scoped-default + custom-`Layer` pattern of `api::masters`' tests.
        pub(super) struct LogCapture {
            seen: Arc<std::sync::Mutex<Vec<(String, String)>>>,
            _guard: tracing::subscriber::DefaultGuard,
        }

        pub(super) fn capture_logs() -> LogCapture {
            use tracing_subscriber::layer::SubscriberExt;

            #[derive(Clone, Default)]
            struct Seen(Arc<std::sync::Mutex<Vec<(String, String)>>>);
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.0 = format!("{value:?}");
                    }
                }
            }
            impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Seen {
                fn on_event(
                    &self,
                    event: &tracing::Event<'_>,
                    _ctx: tracing_subscriber::layer::Context<'_, S>,
                ) {
                    let mut msg = Message(String::new());
                    event.record(&mut msg);
                    self.0
                        .lock()
                        .unwrap()
                        .push((event.metadata().level().to_string(), msg.0));
                }
            }
            let seen = Seen::default();
            let guard =
                tracing::subscriber::set_default(tracing_subscriber::registry().with(seen.clone()));
            tracing::callsite::rebuild_interest_cache();
            LogCapture {
                seen: seen.0,
                _guard: guard,
            }
        }

        impl LogCapture {
            /// The captured events at `levels`, drained.
            pub(super) fn take(&self, levels: &[&str]) -> Vec<String> {
                std::mem::take(&mut *self.seen.lock().unwrap())
                    .into_iter()
                    .filter(|(level, _)| levels.contains(&level.as_str()))
                    .map(|(_, msg)| msg)
                    .collect()
            }
        }

        /// An own row as the publish path records it (R8 adoption shape when
        /// `state = "unknown"`).
        pub(super) fn own_row(uuid: &str, state: &str, landed: &str) -> LocalFrameRow {
            LocalFrameRow {
                project_id: PID.into(),
                frame_uuid: uuid.into(),
                content_version: 1,
                origin: FrameOrigin::Own,
                publisher_account_id: "acc-me".into(),
                publisher_display: "Me".into(),
                file_name: format!("{uuid}.fits"),
                filter_canonical: "L".into(),
                state: state.into(),
                accepted: true,
                byte_size: 7,
                xxh3: "0".repeat(16),
                blake3: "0".repeat(64),
                holder_count: 0,
                manifest_version: 0,
                manifest_json: "{}".into(),
                landed_path: Some(landed.into()),
                size_mtime_seen: Some("7:1".into()),
                on_disk: true,
                locally_declined: false,
                awaiting_gc: false,
                source_frame_id: Some(42),
                recipe_hash: Some("recipe".into()),
                last_error: None,
                updated_at: String::new(),
            }
        }
    }

    #[cfg(all(feature = "render", feature = "solver"))]
    mod poll {
        use super::v3_fx::*;
        use super::*;
        use crate::api::collab::take_gate_moves_seen;
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// Once the project is joined and synced, a poll where nothing moved
        /// costs exactly ONE hub request: `/me/project-versions`.
        #[tokio::test]
        async fn version_poll_is_quiet_when_nothing_moved() {
            let fx = fx("send_receive", false, false).await;
            fx.hub.seed_frames(PID, "acc-o", &["f1"], "published");

            let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(moved, vec![PID.to_string()], "the join moves the project");
            assert_eq!(rows(&fx.ctx).len(), 1);

            for _ in 0..2 {
                let before = request_count(&fx.hub).await;
                let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
                assert!(moved.is_empty(), "nothing moved: {moved:?}");
                assert_eq!(
                    requests_since(&fx.hub, before).await,
                    vec![("GET".to_string(), "/api/v1/me/project-versions".to_string())],
                    "a quiet poll is one request"
                );
            }
        }

        /// A moved version pulls only the delta: the manifest request carries
        /// `since=<old cursor>` and the local rows grow by exactly the new ones.
        #[tokio::test]
        async fn moved_version_pulls_only_the_delta() {
            let fx = fx("send_receive", false, false).await;
            fx.hub
                .seed_frames(PID, "acc-o", &["f1", "f2", "f3"], "published");
            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(rows(&fx.ctx).len(), 3);
            let cursor = project(&fx.ctx).unwrap().manifest_cursor;
            assert_eq!(cursor, fx.hub.frame(PID, "f1").unwrap().manifest_version);

            fx.hub.seed_frames(PID, "acc-o", &["f4", "f5"], "published");
            let before = request_count(&fx.hub).await;
            let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(moved, vec![PID.to_string()]);
            assert_eq!(
                manifest_queries(&fx.hub, before).await,
                vec![(cursor, None)],
                "one manifest request, from the old cursor"
            );
            assert_eq!(rows(&fx.ctx).len(), 5, "the local rows grow by 2");
            let p = project(&fx.ctx).unwrap();
            assert_eq!(
                p.manifest_cursor,
                fx.hub.frame(PID, "f4").unwrap().manifest_version
            );
            assert_eq!(
                p.hub_version,
                fx.hub.state.lock().unwrap().projects[PID].version,
                "the poll vouches for the listed version"
            );
        }

        /// R14: the project disappears from my lists. It is marked lost and
        /// hidden, its collab-store tags (in-flight ones too) and replica rows
        /// go, my own row and file stay. A re-join clears the mark, refetches
        /// the manifest from 0 and re-binds my own row.
        #[tokio::test]
        async fn lost_project_unseeds_and_keeps_own_files() {
            let fx = fx("send_receive", false, false).await;
            let (node, collab) = bind_node_with_collab_root(&fx).await;
            fx.hub.seed_frames(PID, "acc-me", &["fm"], "published");
            fx.hub.seed_frames(PID, "acc-o", &["fr"], "published");
            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(row(&fx.ctx, "fm").unwrap().origin, FrameOrigin::Own);
            assert!(row(&fx.ctx, "fr").is_some());

            // My own file + a landed replica, both seeded; plus an in-flight tag.
            let own_path = collab.join("m31").join("me").join("fm.fits");
            let rep_path = collab.join("m31").join("other").join("fr.fits");
            for (p, byte) in [(&own_path, 1u8), (&rep_path, 2u8)] {
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, vec![byte; 32 * 1024]).unwrap();
            }
            let own_hash = node
                .seed_project_frame(PID, "fm", 1, &own_path)
                .await
                .unwrap();
            node.seed_project_frame(PID, "fr", 1, &rep_path)
                .await
                .unwrap();
            node.collab_store()
                .unwrap()
                .tags()
                .set(
                    format!("in-flight/project/{PID}/fz/1"),
                    iroh_blobs::HashAndFormat::raw(own_hash),
                )
                .await
                .unwrap();
            {
                let conn = db(&fx.ctx).unwrap().conn();
                frames_db::record_own(
                    &conn,
                    &own_row("fm", "published", own_path.to_str().unwrap()),
                )
                .unwrap();
            }
            let project_prefix = format!("project/{PID}/");
            let in_flight_prefix = format!("in-flight/project/{PID}/");
            assert_eq!(collab_tags(&node, &project_prefix).await, 2);
            assert_eq!(collab_tags(&node, &in_flight_prefix).await, 1);

            fx.hub.remove_member(PID, "acc-me");
            let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
            assert!(moved.is_empty(), "a lost project is not a moved one");

            assert_eq!(collab_tags(&node, &project_prefix).await, 0);
            assert_eq!(collab_tags(&node, &in_flight_prefix).await, 0);
            assert!(row(&fx.ctx, "fr").is_none(), "replica rows are deleted");
            let own = row(&fx.ctx, "fm").expect("my own row survives the loss");
            assert_eq!(own.landed_path.as_deref(), own_path.to_str());
            assert!(own_path.exists(), "my own file stays on disk");
            {
                let conn = db(&fx.ctx).unwrap().conn();
                assert!(crate::db::collab::lost_at(&conn, PID).unwrap().is_some());
                assert!(
                    crate::db::collab::list_projects(&conn).unwrap().is_empty(),
                    "a lost project is hidden"
                );
            }
            assert!(crate::api::collab::list_projects(&fx.ctx)
                .unwrap()
                .is_empty());
            let before = request_count(&fx.hub).await;
            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(
                requests_since(&fx.hub, before).await.len(),
                1,
                "a lost project is out of the poll"
            );

            fx.hub.add_member(PID, "acc-me", "send_receive", false);
            let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(moved, vec![PID.to_string()], "a re-join moves the project");
            {
                let conn = db(&fx.ctx).unwrap().conn();
                assert!(crate::db::collab::lost_at(&conn, PID).unwrap().is_none());
            }
            assert!(
                row(&fx.ctx, "fr").is_some(),
                "the refetch from 0 brings replicas back"
            );
            let own = row(&fx.ctx, "fm").unwrap();
            assert_eq!(own.origin, FrameOrigin::Own);
            assert_eq!(own.landed_path.as_deref(), own_path.to_str());
            assert_eq!(own.blake3, fx.hub.frame(PID, "fm").unwrap().blake3);
            node.shutdown().await;
        }

        /// The hook fires when the thresholds version changes, and not on a
        /// move that changed neither thresholds nor dictionary.
        #[tokio::test]
        async fn threshold_move_calls_the_hook() {
            let fx = fx("send_receive", false, false).await;
            poll_versions_once(&fx.ctx, None).await.unwrap();
            take_gate_moves_seen();

            fx.hub.bump(PID);
            let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(moved, vec![PID.to_string()]);
            assert!(
                take_gate_moves_seen().is_empty(),
                "a plain bump is not a thresholds/dictionary move"
            );

            fx.hub.set_thresholds_version(PID, 3);
            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(take_gate_moves_seen(), vec![PID.to_string()], "once");
            assert_eq!(project(&fx.ctx).unwrap().thresholds_version, Some(3));
        }

        /// R11: a thresholds change the UI's project refresh absorbed still
        /// fires the hook — once, from that refresh — and the poll that
        /// follows does not fire it again.
        #[tokio::test]
        async fn a_move_absorbed_by_a_ui_refresh_fires_the_hook_once() {
            let fx = fx("send_receive", false, false).await;
            poll_versions_once(&fx.ctx, None).await.unwrap();
            take_gate_moves_seen();

            fx.hub.set_thresholds_version(PID, 2);
            crate::api::collab::refresh_projects(&fx.ctx).await.unwrap();
            assert_eq!(take_gate_moves_seen(), vec![PID.to_string()]);

            let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(moved, vec![PID.to_string()], "the version still moved");
            assert!(take_gate_moves_seen().is_empty(), "not a second time");
        }

        /// A dictionary move reaches the cache through the refresh the poll
        /// runs, and fires the hook once.
        #[tokio::test]
        async fn dictionary_move_is_refetched_and_calls_the_hook() {
            use crate::collab::filters::DictionaryEntry;
            let fx = fx("send_receive", false, false).await;
            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(project(&fx.ctx).unwrap().dictionary_version, Some(1));
            take_gate_moves_seen();

            fx.hub.set_dictionary(
                PID,
                2,
                vec![DictionaryEntry {
                    canonical: "Sii".into(),
                    aliases: vec!["S2".into()],
                    kind: "narrowband".into(),
                }],
            );
            poll_versions_once(&fx.ctx, None).await.unwrap();
            let p = project(&fx.ctx).unwrap();
            assert_eq!(p.dictionary_version, Some(2));
            let dict: Vec<DictionaryEntry> =
                serde_json::from_str(p.dictionary_json.as_deref().unwrap()).unwrap();
            assert_eq!(dict.len(), 1);
            assert_eq!(dict[0].canonical, "Sii");
            assert_eq!(take_gate_moves_seen(), vec![PID.to_string()]);
        }

        /// R12: a caps change landing between the refresh and the manifest
        /// fetch is still acted on at the next poll — the manifest's own
        /// `projectVersion` (already past the change) is never stored.
        #[tokio::test]
        async fn a_caps_change_between_refresh_and_manifest_is_acted_on_next_poll() {
            let fx = fx("send_receive", false, true).await;
            fx.hub.set_caps(PID, "acc-me", &["data.moderate"]);
            fx.hub.seed_frames(PID, "acc-o", &["fp"], "pending");
            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert!(row(&fx.ctx, "fp").is_some());

            fx.hub.seed_frames(PID, "acc-o", &["fo"], "published");
            fx.hub.before_next("/manifest", |st| {
                let p = st.projects.get_mut(PID).unwrap();
                for m in p.members.iter_mut().filter(|m| m.account_id == "acc-me") {
                    m.gov_caps.clear();
                }
                p.bump();
            });
            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert!(row(&fx.ctx, "fo").is_some());
            assert!(
                row(&fx.ctx, "fp").is_some(),
                "the refresh ran before the revoke"
            );

            let before = request_count(&fx.hub).await;
            let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(moved, vec![PID.to_string()], "the revoke is a move");
            assert_eq!(manifest_queries(&fx.hub, before).await, vec![(0, None)]);
            assert!(row(&fx.ctx, "fp").is_none(), "pruned under the new caps");
        }

        /// A moved project whose refresh failed keeps its old `hub_version`:
        /// its manifest sync waits for a poll whose refresh succeeds, so the
        /// thresholds change behind the move is never skipped.
        #[tokio::test]
        async fn a_failed_refresh_defers_the_manifest_sync() {
            let fx = fx("send_receive", false, false).await;
            poll_versions_once(&fx.ctx, None).await.unwrap();
            let synced = project(&fx.ctx).unwrap().hub_version;

            fx.hub.set_thresholds_version(PID, 2);
            fx.hub
                .set_failing(&format!("/projects/{PID}/thresholds"), true);
            let before = request_count(&fx.hub).await;
            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert!(
                manifest_queries(&fx.hub, before).await.is_empty(),
                "no manifest sync after a failed refresh"
            );
            assert_eq!(project(&fx.ctx).unwrap().hub_version, synced);

            fx.hub
                .set_failing(&format!("/projects/{PID}/thresholds"), false);
            clear_poll_backoff_for(&fx.ctx);
            poll_versions_once(&fx.ctx, None).await.unwrap();
            let p = project(&fx.ctx).unwrap();
            assert_eq!(p.thresholds_version, Some(2));
            assert_eq!(
                p.hub_version,
                fx.hub.state.lock().unwrap().projects[PID].version
            );
        }

        /// R13: a persistently failing project backs off to the pass cadence —
        /// the next tick costs no request beyond the version list and logs
        /// nothing above `debug`; the failure itself is logged once.
        #[tokio::test]
        async fn a_failing_project_backs_off_and_logs_once() {
            let fx = fx("send_receive", false, false).await;
            poll_versions_once(&fx.ctx, None).await.unwrap();

            fx.hub.bump(PID);
            fx.hub
                .set_failing(&format!("/projects/{PID}/thresholds"), true);
            let logs = capture_logs();
            assert!(poll_versions_once(&fx.ctx, None).await.unwrap().is_empty());
            let failures = logs.take(&["WARN", "ERROR"]);
            assert_eq!(failures.len(), 1, "logged once: {failures:?}");

            let before = request_count(&fx.hub).await;
            assert!(poll_versions_once(&fx.ctx, None).await.unwrap().is_empty());
            assert_eq!(
                requests_since(&fx.hub, before).await,
                vec![("GET".to_string(), "/api/v1/me/project-versions".to_string())],
                "a backed-off project costs no request"
            );
            assert!(logs.take(&["WARN", "ERROR", "INFO"]).is_empty());

            fx.hub
                .set_failing(&format!("/projects/{PID}/thresholds"), false);
            clear_poll_backoff_for(&fx.ctx);
            assert_eq!(
                poll_versions_once(&fx.ctx, None).await.unwrap(),
                vec![PID.to_string()],
                "retried once the back-off is over"
            );
        }

        /// Signed out: the poll is a silent `Ok(vec![])`.
        #[tokio::test]
        async fn poll_is_a_no_op_when_signed_out() {
            let (_tmp, ctx) = test_ctx();
            assert!(poll_versions_once(&ctx, None).await.unwrap().is_empty());
        }

        /// Only a poll that moved something kicks the pass.
        #[test]
        fn poll_kicks_the_pass_only_when_a_version_moved() {
            assert!(!kick_if_versions_moved(&[]));
            assert!(kick_if_versions_moved(&["p1".to_string()]));
        }

        /// R15: the version poll ticks on its own loop — nothing gates it.
        #[tokio::test]
        async fn the_version_poll_ticks_on_its_own_loop() {
            let polls = Arc::new(AtomicUsize::new(0));
            let task = tokio::spawn({
                let polls = Arc::clone(&polls);
                version_poll_loop(Duration::ZERO, Duration::from_millis(10), move || {
                    let polls = Arc::clone(&polls);
                    async move {
                        polls.fetch_add(1, Ordering::SeqCst);
                    }
                })
            });
            wait_until(|| polls.load(Ordering::SeqCst) >= 3, Duration::from_secs(5)).await;
            task.abort();
        }

        /// A kick stored before a timer pass (a version move during the
        /// startup grace) is covered by that pass: no second pass right
        /// after. A later kick still wakes the loop.
        #[tokio::test]
        async fn a_kick_before_a_timer_pass_buys_no_second_pass() {
            let passes = Arc::new(AtomicUsize::new(0));
            let kinds = Arc::new(std::sync::Mutex::new(Vec::new()));
            let kick = Arc::new(tokio::sync::Notify::new());
            kick.notify_one();
            let task = tokio::spawn({
                let passes = Arc::clone(&passes);
                let kinds = Arc::clone(&kinds);
                let kick = Arc::clone(&kick);
                async move {
                    auto_sync_loop_inner(
                        Duration::from_millis(20),
                        Duration::from_secs(3600),
                        &kick,
                        move |kind| {
                            let passes = Arc::clone(&passes);
                            let kinds = Arc::clone(&kinds);
                            async move {
                                kinds.lock().unwrap().push(kind);
                                passes.fetch_add(1, Ordering::SeqCst);
                            }
                        },
                    )
                    .await
                }
            });
            wait_until(
                || passes.load(Ordering::SeqCst) >= 1,
                Duration::from_secs(5),
            )
            .await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert_eq!(passes.load(Ordering::SeqCst), 1, "no double pass");
            kick.notify_one();
            wait_until(
                || passes.load(Ordering::SeqCst) >= 2,
                Duration::from_secs(5),
            )
            .await;
            task.abort();
            assert_eq!(
                *kinds.lock().unwrap(),
                vec![PassKind::Cadence, PassKind::Kicked],
                "the timer pass walks the disk; the kicked one is manifest-driven only"
            );
        }
    }

    #[cfg(all(feature = "render", feature = "solver"))]
    mod manifest {
        use super::v3_fx::*;
        use super::*;

        /// Page size 2, five frames sharing ONE manifest version: one
        /// `sync_manifest` follows `next` (the `after` tiebreaker) to the end.
        /// A sync outside the poll vouches for no version (R12).
        #[tokio::test]
        async fn paging_follows_next() {
            let fx = fx("send_receive", false, false).await;
            fx.hub.set_page_size(2);
            fx.hub
                .seed_frames(PID, "acc-o", &["f1", "f2", "f3", "f4", "f5"], "published");
            crate::api::collab::refresh_projects(&fx.ctx).await.unwrap();

            let before = request_count(&fx.hub).await;
            sync_manifest(&fx.ctx, PID, None, None).await.unwrap();
            assert_eq!(rows(&fx.ctx).len(), 5, "all five stored");
            let mv = fx.hub.frame(PID, "f1").unwrap().manifest_version;
            assert_eq!(
                manifest_queries(&fx.hub, before).await,
                vec![
                    (0, None),
                    (mv, Some("f2".to_string())),
                    (mv, Some("f4".to_string())),
                ]
            );
            let p = project(&fx.ctx).unwrap();
            assert_eq!(p.manifest_cursor, mv);
            assert_eq!(p.hub_version, 0, "no vouched version, no hub_version");
        }

        /// Losing `data.moderate` re-fetches from 0 and prunes the pending row
        /// of another member I can no longer see; own rows survive.
        #[tokio::test]
        async fn caps_change_refetches_from_zero_and_prunes() {
            let fx = fx("send_receive", false, true).await;
            fx.hub.set_caps(PID, "acc-me", &["data.moderate"]);
            fx.hub.seed_frames(PID, "acc-o", &["fp"], "pending");
            fx.hub.seed_frames(PID, "acc-o", &["fo"], "published");
            fx.hub.seed_frames(PID, "acc-me", &["fm"], "published");

            poll_versions_once(&fx.ctx, None).await.unwrap();
            assert!(
                row(&fx.ctx, "fp").is_some(),
                "a moderator sees the pending row"
            );
            assert_eq!(
                project(&fx.ctx).unwrap().synced_caps_json,
                r#"["data.moderate"]"#
            );
            // An own row the hub does not list (yet) must survive the prune too.
            {
                let conn = db(&fx.ctx).unwrap().conn();
                frames_db::record_own(&conn, &own_row("fx", "unknown", "/nowhere/fx.fits"))
                    .unwrap();
            }

            fx.hub.set_caps(PID, "acc-me", &[]);
            let before = request_count(&fx.hub).await;
            let moved = poll_versions_once(&fx.ctx, None).await.unwrap();
            assert_eq!(moved, vec![PID.to_string()]);
            assert_eq!(
                manifest_queries(&fx.hub, before).await,
                vec![(0, None)],
                "a caps change refetches from 0"
            );
            assert!(row(&fx.ctx, "fp").is_none(), "the pending row is pruned");
            assert!(row(&fx.ctx, "fo").is_some());
            assert_eq!(row(&fx.ctx, "fm").unwrap().origin, FrameOrigin::Own);
            assert!(row(&fx.ctx, "fx").is_some(), "own rows are never pruned");
            assert_eq!(project(&fx.ctx).unwrap().synced_caps_json, "[]");
        }

        /// The refresh stores the hub's pending count and this member's caps.
        #[tokio::test]
        async fn refresh_stores_pending_frames_and_caps() {
            let fx = fx("send_receive", false, true).await;
            fx.hub.set_caps(PID, "acc-me", &["data.moderate"]);
            fx.hub.seed_frames(PID, "acc-o", &["p1", "p2"], "pending");
            crate::api::collab::refresh_projects(&fx.ctx).await.unwrap();
            let p = project(&fx.ctx).unwrap();
            assert_eq!(p.pending_frames, 2);
            assert_eq!(p.gov_caps_json, r#"["data.moderate"]"#);
            assert_eq!(p.synced_caps_json, "[]", "only a manifest sync writes it");
        }

        /// A coordinator carries a `"coordinator"` caps element, so a
        /// coordinator flip is a caps change too.
        #[tokio::test]
        async fn coordinator_is_part_of_the_caps() {
            let fx = fx("send_receive", true, false).await;
            crate::api::collab::refresh_projects(&fx.ctx).await.unwrap();
            assert_eq!(
                project(&fx.ctx).unwrap().gov_caps_json,
                r#"["coordinator"]"#
            );
        }

        /// Every `FramesChangeKind` driven through the fake, each emitted once
        /// with its count.
        #[tokio::test]
        async fn change_kinds_are_classified() {
            let fx = fx("send_receive", false, true).await;
            fx.hub.set_caps(PID, "acc-me", &["data.moderate"]);
            fx.hub.seed_frames(PID, "acc-me", &["a", "b"], "pending");
            fx.hub.seed_frames(PID, "acc-o", &["c", "d"], "published");
            crate::api::collab::refresh_projects(&fx.ctx).await.unwrap();

            let first = RecordingEmitter::default();
            sync_manifest(&fx.ctx, PID, Some(&first), None)
                .await
                .unwrap();
            assert_eq!(first.frame_changes(), vec![("newFrames".to_string(), 2)]);

            fx.hub.seed_frames(PID, "acc-o", &["e"], "published");
            fx.hub.seed_frames(PID, "acc-o", &["f"], "pending");
            fx.hub
                .update_frame(PID, "a", |f| f.state = "published".into());
            fx.hub.update_frame(PID, "b", |f| {
                f.state = "rejected".into();
                f.reject_reason = Some("soft".into());
            });
            fx.hub.set_accepted(PID, "d", false, Some("trailing"));
            fx.hub.update_frame(PID, "c", |f| {
                f.content_version = 2;
                f.blake3 = "c".repeat(64);
            });

            let rec = RecordingEmitter::default();
            let changes = sync_manifest(&fx.ctx, PID, Some(&rec), None).await.unwrap();
            let expected: Vec<(String, u64)> = [
                "approved",
                "excluded",
                "newFrames",
                "newVersions",
                "pendingFrames",
                "rejected",
            ]
            .iter()
            .map(|k| (k.to_string(), 1))
            .collect();
            assert_eq!(rec.frame_changes(), expected, "one event per kind");
            assert_eq!(changes.len(), 6);
            assert!(changes.iter().all(|c| c.count == 1 && c.project_id == PID));
        }

        /// Syncs of one project are serialized: two running in parallel (the
        /// poll's and the pass's, on two worker threads) apply a delta once,
        /// so its events are emitted once.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn concurrent_syncs_emit_a_delta_once() {
            let Fx {
                tmp: _tmp,
                ctx,
                hub,
            } = fx("send_receive", false, false).await;
            crate::api::collab::refresh_projects(&ctx).await.unwrap();
            let uuids: Vec<String> = (0..300).map(|i| format!("f{i:03}")).collect();
            let refs: Vec<&str> = uuids.iter().map(String::as_str).collect();
            hub.seed_frames(PID, "acc-o", &refs, "published");

            let ctx = Arc::new(ctx);
            let rec = Arc::new(RecordingEmitter::default());
            let tasks: Vec<_> = (0..4)
                .map(|_| {
                    let ctx = Arc::clone(&ctx);
                    let rec = Arc::clone(&rec);
                    tokio::spawn(async move {
                        let em: &dyn ProgressEmitter = &*rec;
                        sync_manifest(&ctx, PID, Some(em), None).await.unwrap();
                    })
                })
                .collect();
            for t in tasks {
                t.await.unwrap();
            }
            assert_eq!(rec.frame_changes(), vec![("newFrames".to_string(), 300)]);
        }

        /// R8: an adopted own row (state `unknown`, LOCAL hashes at v1,
        /// `accepted` false) takes the hub's columns from the manifest while
        /// its local columns stay.
        #[tokio::test]
        async fn manifest_sync_corrects_an_adopted_own_row() {
            let fx = fx("send_receive", false, false).await;
            fx.hub.seed_frames(PID, "acc-me", &["fa"], "published");
            fx.hub.update_frame(PID, "fa", |f| {
                f.content_version = 2;
                f.byte_size = 4242;
            });
            crate::api::collab::refresh_projects(&fx.ctx).await.unwrap();
            {
                let conn = db(&fx.ctx).unwrap().conn();
                let mut adopted = own_row("fa", "unknown", "/landed/fa.fits");
                adopted.accepted = false;
                frames_db::record_own(&conn, &adopted).unwrap();
            }

            sync_manifest(&fx.ctx, PID, None, None).await.unwrap();

            let hub = fx.hub.frame(PID, "fa").unwrap();
            let r = row(&fx.ctx, "fa").unwrap();
            assert_eq!(r.origin, FrameOrigin::Own);
            assert_eq!(r.state, "published");
            assert!(r.accepted, "accepted is hub state");
            assert_eq!(r.content_version, 2);
            assert_eq!(r.blake3, hub.blake3);
            assert_eq!(r.xxh3, hub.xxh3);
            assert_eq!(r.byte_size, 4242);
            assert_eq!(r.manifest_version, hub.manifest_version);
            assert_eq!(r.landed_path.as_deref(), Some("/landed/fa.fits"));
            assert_eq!(r.source_frame_id, Some(42));
            assert_eq!(r.recipe_hash.as_deref(), Some("recipe"));
            assert!(r.on_disk, "an own row keeps on_disk across a version move");
        }

        /// The pure classifier, one row at a time.
        #[test]
        fn classify_frame_change_rules() {
            use FramesChangeKind as K;
            let view = |own: bool, state: &str, accepted: bool, cv: i32| FrameViewWire {
                frame_uuid: "u".into(),
                publisher_account_id: "a".into(),
                publisher_display_name: "A".into(),
                own,
                file_name: "u.fits".into(),
                content_version: cv,
                blake3: "0".repeat(64),
                byte_size: 1,
                xxh3: "0".repeat(16),
                filter_raw: "L".into(),
                filter_canonical: "L".into(),
                channel: "mono".into(),
                exptime_sec: 1.0,
                date_obs: None,
                meta: serde_json::json!({}),
                gate_version: 0,
                accepted,
                accepted_reason: None,
                state: state.into(),
                reject_reason: None,
                manifest_version: 1,
                created_at: String::new(),
                holder_count: 0,
            };
            let prev = |origin: FrameOrigin, state: &str, accepted: bool, cv: i32| {
                let mut r = own_row("u", state, "/x");
                r.origin = origin;
                r.accepted = accepted;
                r.content_version = cv;
                r
            };
            let rep = FrameOrigin::Replica;
            let own = FrameOrigin::Own;

            assert_eq!(
                classify_frame_change(None, &view(false, "published", true, 1)),
                vec![K::NewFrames]
            );
            assert_eq!(
                classify_frame_change(None, &view(false, "pending", true, 1)),
                vec![K::PendingFrames]
            );
            assert!(classify_frame_change(None, &view(true, "published", true, 1)).is_empty());
            assert_eq!(
                classify_frame_change(
                    Some(&prev(rep, "pending", true, 1)),
                    &view(false, "published", true, 1)
                ),
                vec![K::NewFrames],
                "a pending row another moderator approved is new to me"
            );
            assert_eq!(
                classify_frame_change(
                    Some(&prev(own, "pending", true, 1)),
                    &view(true, "published", true, 1)
                ),
                vec![K::Approved]
            );
            assert_eq!(
                classify_frame_change(
                    Some(&prev(own, "pending", true, 1)),
                    &view(true, "rejected", true, 1)
                ),
                vec![K::Rejected]
            );
            assert_eq!(
                classify_frame_change(
                    Some(&prev(rep, "published", true, 1)),
                    &view(false, "published", false, 1)
                ),
                vec![K::Excluded]
            );
            assert_eq!(
                classify_frame_change(
                    Some(&prev(rep, "published", true, 1)),
                    &view(false, "published", true, 2)
                ),
                vec![K::NewVersions]
            );
            assert!(
                classify_frame_change(
                    Some(&prev(own, "published", true, 1)),
                    &view(true, "published", true, 2)
                )
                .is_empty(),
                "my own new version is not news to me"
            );
            assert!(
                classify_frame_change(
                    Some(&prev(rep, "published", true, 1)),
                    &view(false, "published", true, 1)
                )
                .is_empty(),
                "an unchanged row is no change"
            );
        }
    }

    // ── Task 9 (wave 2): the need set, pure ──────────────────────────────────

    mod need {
        use super::*;

        pub(super) fn replica(uuid: &str, holders: i64, created: &str, size: i64) -> LocalFrameRow {
            LocalFrameRow {
                project_id: "p1".into(),
                frame_uuid: uuid.into(),
                content_version: 1,
                origin: FrameOrigin::Replica,
                publisher_account_id: "acc-o".into(),
                publisher_display: "Other".into(),
                file_name: format!("{uuid}.fits"),
                filter_canonical: "L".into(),
                state: "published".into(),
                accepted: true,
                byte_size: size,
                xxh3: "0".repeat(16),
                blake3: "0".repeat(64),
                holder_count: holders,
                manifest_version: 1,
                manifest_json: serde_json::json!({ "createdAt": created, "meta": {} }).to_string(),
                landed_path: None,
                size_mtime_seen: None,
                on_disk: false,
                locally_declined: false,
                awaiting_gc: false,
                source_frame_id: None,
                recipe_hash: None,
                last_error: None,
                updated_at: String::new(),
            }
        }

        fn uuids(v: &[LocalFrameRow]) -> Vec<&str> {
            v.iter().map(|r| r.frame_uuid.as_str()).collect()
        }

        fn all() -> ReplicationPolicy {
            ReplicationPolicy::default()
        }

        /// Spec §5.3: only a published, accepted peer frame that is not on
        /// disk, not declined and not waiting for GC is needed.
        #[test]
        fn need_excludes_own_declined_unaccepted_pending_and_awaiting_gc() {
            let ok = replica("ok", 1, "t", 10);
            let mut own = replica("own", 1, "t", 10);
            own.origin = FrameOrigin::Own;
            let mut declined = replica("declined", 1, "t", 10);
            declined.locally_declined = true;
            let mut unaccepted = replica("unaccepted", 1, "t", 10);
            unaccepted.accepted = false;
            let mut pending = replica("pending", 1, "t", 10);
            pending.state = "pending".into();
            let mut rejected = replica("rejected", 1, "t", 10);
            rejected.state = "rejected".into();
            let mut awaiting = replica("awaiting", 1, "t", 10);
            awaiting.awaiting_gc = true;
            let mut held = replica("held", 1, "t", 10);
            held.on_disk = true;
            let rows = vec![
                ok, own, declined, unaccepted, pending, rejected, awaiting, held,
            ];
            assert_eq!(
                uuids(&frame_need(&rows, &all(), true, true, false)),
                vec!["ok"]
            );
        }

        /// Rarest first (`holderCount`), then oldest (`createdAt`).
        #[test]
        fn need_is_rarest_first_then_oldest() {
            let rows = vec![
                replica("a", 3, "2026-09-01T00:00:01Z", 10),
                replica("b", 1, "2026-09-01T00:00:03Z", 10),
                replica("c", 1, "2026-09-01T00:00:02Z", 10),
                replica("d", 2, "2026-09-01T00:00:00Z", 10),
            ];
            assert_eq!(
                uuids(&frame_need(&rows, &all(), true, true, false)),
                vec!["c", "b", "d", "a"]
            );
        }

        /// The budget counts the replicas already on disk (own frames are not
        /// replicas) and stops at the first frame that would cross it.
        #[test]
        fn byte_budget_counts_already_held_bytes() {
            let mut held = replica("held", 1, "t0", 600);
            held.on_disk = true;
            let mut own = replica("own", 1, "t0", 5000);
            own.origin = FrameOrigin::Own;
            own.on_disk = true;
            let rows = vec![
                held,
                own,
                replica("a", 1, "t1", 300),
                replica("b", 1, "t2", 200),
                replica("c", 1, "t3", 10),
            ];
            let policy = ReplicationPolicy {
                byte_budget: Some(1000),
                ..all()
            };
            assert_eq!(
                uuids(&frame_need(&rows, &policy, true, true, false)),
                vec!["a"],
                "600 held + 300 fits, + 200 would not — and the walk stops there"
            );
            let roomy = ReplicationPolicy {
                byte_budget: Some(1100),
                ..all()
            };
            assert_eq!(
                uuids(&frame_need(&rows, &roomy, true, true, false)),
                vec!["a", "b"]
            );
        }

        /// Filters, publishers, FWHM and star bounds each narrow the set; a
        /// frame without the measurement never matches a set bound.
        #[test]
        fn policy_filters_by_canonical_filter_publisher_fwhm_stars() {
            let with_meta = |uuid: &str, filter: &str, publisher: &str, meta: serde_json::Value| {
                let mut r = replica(uuid, 1, uuid, 10);
                r.filter_canonical = filter.into();
                r.publisher_account_id = publisher.into();
                r.manifest_json =
                    serde_json::json!({ "createdAt": uuid, "meta": meta }).to_string();
                r
            };
            let rows = vec![
                with_meta(
                    "1",
                    "L",
                    "acc-o",
                    serde_json::json!({ "fwhmArcsec": 2.0, "starsDetected": 100 }),
                ),
                with_meta(
                    "2",
                    "Ha",
                    "acc-o",
                    serde_json::json!({ "fwhmArcsec": 4.0, "starsDetected": 10 }),
                ),
                with_meta("3", "L", "acc-x", serde_json::json!({})),
            ];
            let need = |p: ReplicationPolicy| {
                uuids(&frame_need(&rows, &p, true, true, false))
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            };
            assert_eq!(need(all()), vec!["1", "2", "3"]);
            assert_eq!(
                need(ReplicationPolicy {
                    filters: vec!["Ha".into()],
                    ..all()
                }),
                vec!["2"]
            );
            assert_eq!(
                need(ReplicationPolicy {
                    publishers: vec!["acc-x".into()],
                    ..all()
                }),
                vec!["3"]
            );
            assert_eq!(
                need(ReplicationPolicy {
                    max_fwhm_arcsec: Some(3.0),
                    ..all()
                }),
                vec!["1"],
                "4.0 is over the bound and a missing FWHM never matches"
            );
            assert_eq!(
                need(ReplicationPolicy {
                    min_stars: Some(50),
                    ..all()
                }),
                vec!["1"]
            );
        }

        /// A paused project, a role that may not replicate, and an
        /// auto-replicate toggle that is off all need nothing.
        #[test]
        fn paused_needs_nothing() {
            let rows = vec![replica("a", 1, "t", 10)];
            assert!(frame_need(&rows, &all(), true, true, true).is_empty());
            assert!(frame_need(&rows, &all(), false, true, false).is_empty());
            assert!(frame_need(&rows, &all(), true, false, false).is_empty());
            assert_eq!(frame_need(&rows, &all(), true, true, false).len(), 1);
        }
    }

    // ── Task 9 (wave 2): the replication pass against the fake hub ───────────

    #[cfg(all(feature = "render", feature = "solver"))]
    mod replication {
        use super::v3_fx::*;
        use super::*;
        use crate::sharing::iroh::node::{BlobHealth, SharedIrohNode};

        /// The v3 fixture with the context in an `Arc` (the loss command
        /// takes one), the project refreshed into the cache and the
        /// Collaboration root `<tmp>/Collab` designated. No node is bound.
        pub(super) struct RFx {
            pub tmp: tempfile::TempDir,
            pub ctx: Arc<ServiceContext>,
            pub hub: FakeHub,
            pub collab: PathBuf,
        }

        pub(super) async fn rfx(role: &str) -> RFx {
            let Fx { tmp, ctx, hub } = fx(role, false, false).await;
            crate::api::collab::refresh_projects(&ctx).await.unwrap();
            let requested = tmp.path().join("Collab");
            std::fs::create_dir_all(&requested).unwrap();
            let collab = PathBuf::from(
                crate::api::scan_roots::set_collaboration_dir(
                    &ctx,
                    requested.to_string_lossy().to_string(),
                    &crate::api::PathPolicy::AllowAll,
                )
                .await
                .unwrap(),
            );
            RFx {
                tmp,
                ctx: Arc::new(ctx),
                hub,
                collab,
            }
        }

        /// A recording fetch seam: `(project, frame uuids)` per call; errs
        /// for the projects in `fail`.
        #[derive(Default)]
        pub(super) struct FetchRecorder {
            pub calls: std::sync::Mutex<Vec<(String, Vec<String>)>>,
            pub fail: std::sync::Mutex<HashSet<String>>,
        }

        impl FetchRecorder {
            pub(super) fn projects(&self) -> Vec<String> {
                self.calls
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(p, _)| p.clone())
                    .collect()
            }
        }

        pub(super) async fn pass_with(
            ctx: &ServiceContext,
            rec: &Arc<FetchRecorder>,
            kind: PassKind,
            scope: Option<&str>,
            emitter: Option<&dyn ProgressEmitter>,
        ) -> AutoSyncPassOutcome {
            let rec = Arc::clone(rec);
            run_auto_sync_pass(ctx, kind, scope, emitter, move |project_id, need| {
                let rec = Arc::clone(&rec);
                async move {
                    let uuids: Vec<String> = need.iter().map(|r| r.frame_uuid.clone()).collect();
                    let n = uuids.len();
                    rec.calls.lock().unwrap().push((project_id.clone(), uuids));
                    if rec.fail.lock().unwrap().contains(&project_id) {
                        return Err(ApiError::Internal(format!("fetch {project_id} exploded")));
                    }
                    Ok(FetchOutcome {
                        landed: n,
                        ..Default::default()
                    })
                }
            })
            .await
        }

        /// Write `bytes` under the Collaboration root and record a landed row
        /// for it (`origin`), as a landing or a publish would have.
        pub(super) fn land_file(r: &RFx, uuid: &str, origin: FrameOrigin, bytes: &[u8]) -> PathBuf {
            let dir = r.collab.join("m31").join("other");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("{uuid}.fits"));
            std::fs::write(&path, bytes).unwrap();
            let meta = std::fs::metadata(&path).unwrap();
            let mut row = own_row(uuid, "published", &path.to_string_lossy());
            row.origin = origin;
            if origin == FrameOrigin::Replica {
                row.publisher_account_id = "acc-o".into();
                row.publisher_display = "Other".into();
                row.source_frame_id = None;
                row.recipe_hash = None;
            }
            row.byte_size = bytes.len() as i64;
            row.xxh3 = hash_bytes(bytes);
            row.blake3 = blake3::hash(bytes).to_hex().to_string();
            row.size_mtime_seen = Some(size_mtime_from(&meta));
            frames_db::record_own(&db(&r.ctx).unwrap().conn(), &row).unwrap();
            path
        }

        /// Move a file's mtime `secs` into the future.
        pub(super) fn bump_mtime(path: &Path, secs: u64) {
            let t =
                std::fs::metadata(path).unwrap().modified().unwrap() + Duration::from_secs(secs);
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(t)
                .unwrap();
        }

        /// A relay-disabled node installed on `r.ctx` with the Collaboration
        /// root mounted.
        pub(super) async fn bind_receiver(r: &RFx) -> Arc<SharedIrohNode> {
            let dirs = crate::api::sync::sync_dirs(&r.ctx).unwrap();
            std::fs::create_dir_all(&dirs.identity_dir).unwrap();
            std::fs::create_dir_all(&dirs.working_dir).unwrap();
            let node = SharedIrohNode::bind_with(
                &dirs.identity_dir,
                &dirs.working_dir,
                iroh::RelayMode::Disabled,
                crate::sharing::iroh::node::NodeOptions::default(),
            )
            .await
            .expect("bind relay-disabled node");
            *r.ctx.iroh_node.lock().await = Some(Arc::clone(&node));
            node.set_collab_root(Some(&r.collab)).await.unwrap();
            node
        }

        /// A bare relay-disabled node with its own Collaboration root
        /// mounted — another member's app, reduced to its store.
        pub(super) async fn peer_node(dir: &Path) -> Arc<SharedIrohNode> {
            let node = SharedIrohNode::bind(&dir.join("sync"), iroh::RelayMode::Disabled)
                .await
                .unwrap();
            let root = dir.join("Collab");
            std::fs::create_dir_all(&root).unwrap();
            node.set_collab_root(Some(&root)).await.unwrap();
            node
        }

        /// Relay-disabled nodes have no discovery: exchange addresses.
        pub(super) async fn pair(a: &Arc<SharedIrohNode>, b: &Arc<SharedIrohNode>) {
            for n in [a, b] {
                n.handle(Role::Out).start().await.unwrap();
            }
            a.add_peer(b.endpoint_addr());
            b.add_peer(a.endpoint_addr());
        }

        /// `acc-o` publishes `bytes` as `uuid` / `name`: seeded by reference
        /// on `publisher`, listed by the hub with `publisher` as its holder.
        pub(super) async fn publish_on(
            r: &RFx,
            publisher: &Arc<SharedIrohNode>,
            uuid: &str,
            name: &str,
            bytes: &[u8],
        ) -> iroh_blobs::Hash {
            let dir = r.tmp.path().join(format!("pub-{uuid}"));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            let hash = publisher
                .seed_project_frame(PID, uuid, 1, &path)
                .await
                .expect("the publisher seeds its frame");
            set_publisher_key(r, publisher);
            r.hub.seed_frames(PID, "acc-o", &[uuid], "published");
            let (name, xxh3, size) = (name.to_string(), hash_bytes(bytes), bytes.len() as i64);
            r.hub.update_frame(PID, uuid, move |f| {
                f.file_name = name;
                f.blake3 = hash.to_hex().to_string();
                f.xxh3 = xxh3;
                f.byte_size = size;
            });
            hash
        }

        /// `acc-o`'s device is `publisher`.
        pub(super) fn set_publisher_key(r: &RFx, publisher: &Arc<SharedIrohNode>) {
            r.hub.add_account(
                "tok-o",
                "acc-o",
                "Other",
                &B64.encode(publisher.node_id()),
                None,
            );
        }

        pub(super) fn dir_bytes(dir: &Path) -> u64 {
            let mut total = 0;
            if let Ok(entries) = std::fs::read_dir(dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        total += dir_bytes(&p);
                    } else if let Ok(m) = p.metadata() {
                        total += m.len();
                    }
                }
            }
            total
        }

        pub(super) fn pattern(seed: usize, len: usize) -> Vec<u8> {
            (0..len)
                .map(|j| ((j * 31 + seed * 97) % 251) as u8)
                .collect()
        }

        pub(super) async fn real_pass(r: &RFx, kind: PassKind) -> AutoSyncPassOutcome {
            let sync = crate::sync::SyncRuntime::new();
            tokio::time::timeout(
                Duration::from_secs(120),
                replication_pass(&r.ctx, &sync, kind, None, None),
            )
            .await
            .expect("a replication pass must not take two minutes")
        }

        pub(super) async fn sync_rows(r: &RFx) {
            sync_manifest(&r.ctx, PID, None, None).await.unwrap();
        }

        // ── disk truth ───────────────────────────────────────────────────────

        /// A deleted replica is missing: `on_disk` drops, its seed tags go,
        /// and its store entry (dead now) makes it wait for GC (P20).
        #[tokio::test]
        async fn deleted_replica_is_missing_and_unseeded() {
            let r = rfx("send_receive").await;
            let node = bind_receiver(&r).await;
            let path = land_file(&r, "f1", FrameOrigin::Replica, &pattern(1, 64 * 1024));
            node.seed_project_frame(PID, "f1", 1, &path).await.unwrap();
            assert_eq!(collab_tags(&node, "project/p1/").await, 1);

            std::fs::remove_file(&path).unwrap();
            let truth = disk_truth(&r.ctx, PID).await.unwrap();

            assert_eq!(truth.missing_replicas, vec!["f1".to_string()]);
            assert!(truth.present.is_empty());
            assert_eq!(truth.held_replicas, 1);
            assert_eq!(truth.missing_bytes, 64 * 1024);
            let row = row(&r.ctx, "f1").unwrap();
            assert!(!row.on_disk);
            assert!(
                row.awaiting_gc,
                "the entry still reads the deleted path: fetch only after GC"
            );
            assert_eq!(collab_tags(&node, "project/p1/").await, 0, "unseeded");
            node.shutdown().await;
        }

        /// Same size, new mtime, different bytes: an edit counts as missing.
        #[tokio::test]
        async fn edited_file_counts_as_missing() {
            let r = rfx("send_receive").await;
            let path = land_file(&r, "f1", FrameOrigin::Replica, &pattern(1, 4096));
            std::fs::write(&path, pattern(2, 4096)).unwrap();
            bump_mtime(&path, 10);

            let truth = disk_truth(&r.ctx, PID).await.unwrap();
            assert_eq!(truth.rehashed, 1);
            assert_eq!(truth.missing_replicas, vec!["f1".to_string()]);
            let row = row(&r.ctx, "f1").unwrap();
            assert!(!row.on_disk);
            assert!(row.awaiting_gc, "no store to ask: wait, never fetch blind");
        }

        /// A touched file with the same bytes stays held; its new
        /// `size:mtime` is recorded so the next walk does not re-hash it.
        #[tokio::test]
        async fn touched_but_identical_file_is_kept_and_mtime_updated() {
            let r = rfx("send_receive").await;
            let path = land_file(&r, "f1", FrameOrigin::Replica, &pattern(1, 4096));
            let before = row(&r.ctx, "f1").unwrap().size_mtime_seen;
            bump_mtime(&path, 10);

            let truth = disk_truth(&r.ctx, PID).await.unwrap();
            assert_eq!(truth.rehashed, 1);
            assert_eq!(truth.present, vec![("f1".to_string(), 1)]);
            assert!(truth.missing_replicas.is_empty());
            let row = row(&r.ctx, "f1").unwrap();
            assert!(row.on_disk);
            assert_ne!(row.size_mtime_seen, before);
            assert_eq!(
                row.size_mtime_seen.as_deref(),
                Some(size_mtime_from(&std::fs::metadata(&path).unwrap()).as_str())
            );
            let again = disk_truth(&r.ctx, PID).await.unwrap();
            assert_eq!(again.rehashed, 0, "the new size:mtime is trusted");
        }

        /// My own deleted file is missing too — and never needed: own frames
        /// are never fetched back.
        #[tokio::test]
        async fn deleted_own_file_is_missing_own_and_never_needed() {
            let r = rfx("send_receive").await;
            let path = land_file(&r, "f1", FrameOrigin::Own, &pattern(1, 4096));
            std::fs::remove_file(&path).unwrap();

            let truth = disk_truth(&r.ctx, PID).await.unwrap();
            assert_eq!(truth.missing_own, vec!["f1".to_string()]);
            assert!(truth.missing_replicas.is_empty());
            let rows = rows(&r.ctx);
            assert!(!rows[0].on_disk);
            assert!(!rows[0].awaiting_gc);
            assert!(frame_need(&rows, &ReplicationPolicy::default(), true, true, false).is_empty());
        }

        // ── the loss guard ───────────────────────────────────────────────────

        fn paused(r: &RFx) -> bool {
            project(&r.ctx).unwrap().replication_paused
        }

        fn paused_events(em: &RecordingEmitter) -> Vec<serde_json::Value> {
            em.events
                .lock()
                .unwrap()
                .iter()
                .filter(|(n, _)| n == COLLAB_REPLICATION_PAUSED_EVENT)
                .map(|(_, p)| p.clone())
                .collect()
        }

        /// Land `n` replicas and delete the first `lose`.
        fn land_and_lose(r: &RFx, n: usize, lose: usize, size: usize) {
            for i in 0..n {
                let path = land_file(
                    r,
                    &format!("f{i:02}"),
                    FrameOrigin::Replica,
                    &pattern(i, size),
                );
                if i < lose {
                    std::fs::remove_file(&path).unwrap();
                }
            }
        }

        /// One deleted replica never trips the guard, whatever the fraction.
        #[tokio::test]
        async fn one_deleted_replica_does_not_trip_the_guard() {
            let r = rfx("send_receive").await;
            land_and_lose(&r, 3, 1, 256);
            let em = RecordingEmitter::default();
            pass_with(&r.ctx, &Arc::default(), PassKind::Cadence, None, Some(&em)).await;
            assert!(!paused(&r));
            assert!(paused_events(&em).is_empty());
            assert!(!row(&r.ctx, "f00").unwrap().on_disk, "still missing");
        }

        /// 2 of 18 held replicas (11 %) gone: paused, one event, one warn.
        #[tokio::test]
        async fn eleven_percent_missing_trips_and_pauses() {
            let r = rfx("send_receive").await;
            land_and_lose(&r, 18, 2, 256);
            let em = RecordingEmitter::default();
            let logs = capture_logs();
            pass_with(&r.ctx, &Arc::default(), PassKind::Cadence, None, Some(&em)).await;
            assert!(paused(&r));
            let events = paused_events(&em);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0]["projectId"], PID);
            assert_eq!(events[0]["missing"], 2);
            assert_eq!(events[0]["missingBytes"], 512);
            assert!(logs
                .take(&["WARN"])
                .iter()
                .any(|m| m.contains("replication paused by the loss guard")));

            // Already paused: the guard does not fire again.
            let em2 = RecordingEmitter::default();
            std::fs::remove_file(row(&r.ctx, "f05").unwrap().landed_path.unwrap()).unwrap();
            std::fs::remove_file(row(&r.ctx, "f06").unwrap().landed_path.unwrap()).unwrap();
            pass_with(&r.ctx, &Arc::default(), PassKind::Cadence, None, Some(&em2)).await;
            assert!(paused_events(&em2).is_empty());
        }

        /// The byte threshold trips even when the fraction does not (2 of 30
        /// is 6.7 %).
        #[tokio::test]
        async fn bytes_threshold_trips_below_the_fraction() {
            let r = rfx("send_receive").await;
            {
                let conn = db(&r.ctx).unwrap().conn();
                r.ctx
                    .settings
                    .persist_setting(&conn, crate::settings::keys::COLLAB_LOSS_GUARD_BYTES, "100")
                    .unwrap();
            }
            land_and_lose(&r, 30, 2, 64);
            pass_with(&r.ctx, &Arc::default(), PassKind::Cadence, None, None).await;
            assert!(paused(&r), "128 bytes missing > 100");
        }

        /// "Stop holding" declines exactly the missing replicas and unpauses.
        #[tokio::test]
        async fn stop_holding_declines_the_missing_and_unpauses() {
            let r = rfx("send_receive").await;
            land_and_lose(&r, 18, 2, 256);
            pass_with(&r.ctx, &Arc::default(), PassKind::Cadence, None, None).await;
            assert!(paused(&r));

            resolve_collab_loss(
                Arc::clone(&r.ctx),
                Arc::new(crate::sync::SyncRuntime::new()),
                PID,
                LossAction::StopHolding,
                None,
            )
            .await
            .unwrap();

            assert!(!paused(&r));
            for row in rows(&r.ctx) {
                let lost = row.frame_uuid == "f00" || row.frame_uuid == "f01";
                assert_eq!(row.locally_declined, lost, "{}", row.frame_uuid);
            }
            assert!(
                resolve_collab_loss(
                    Arc::clone(&r.ctx),
                    Arc::new(crate::sync::SyncRuntime::new()),
                    "no-such-project",
                    LossAction::StopHolding,
                    None,
                )
                .await
                .is_err(),
                "an unknown project is refused"
            );
        }

        // ── holder reports ───────────────────────────────────────────────────

        fn holder_puts(reqs: &[wiremock::Request]) -> Vec<serde_json::Value> {
            reqs.iter()
                .filter(|q| q.method.as_str() == "PUT" && q.url.path().ends_with("/holders/self"))
                .map(|q| q.body_json::<serde_json::Value>().unwrap())
                .collect()
        }

        /// F5 regression: the full report comes from the disk, so a deleted
        /// own file stops being a holder at the hub on the next cadence pass.
        #[tokio::test]
        async fn full_report_carries_only_present_frames() {
            let r = rfx("send_receive").await;
            r.hub.seed_frames(PID, "acc-me", &["f1", "f2"], "published");
            sync_rows(&r).await;
            let me = B64.encode(own_node_for(&r.ctx));
            assert_eq!(
                r.hub.holders_of(PID, "f2"),
                vec![me.clone()],
                "announce made me a holder"
            );
            let dir = r.collab.join("m31").join("me");
            std::fs::create_dir_all(&dir).unwrap();
            for uuid in ["f1", "f2"] {
                let path = dir.join(format!("{uuid}.fits"));
                std::fs::write(&path, uuid).unwrap();
                let sm = size_mtime_from(&std::fs::metadata(&path).unwrap());
                frames_db::adopt_own(
                    &db(&r.ctx).unwrap().conn(),
                    PID,
                    uuid,
                    1,
                    &path.to_string_lossy(),
                    "recipe",
                    Some(&sm),
                )
                .unwrap();
            }
            std::fs::remove_file(dir.join("f2.fits")).unwrap();

            let from = request_count(&r.hub).await;
            let out = pass_with(&r.ctx, &Arc::default(), PassKind::Cadence, None, None).await;
            assert_eq!(out.reported, 1);
            let reqs = r.hub.server.received_requests().await.unwrap();
            let puts = holder_puts(&reqs[from..]);
            assert_eq!(puts.len(), 1);
            assert_eq!(puts[0]["full"], true);
            assert_eq!(
                puts[0]["add"],
                serde_json::json!([{ "frameUuid": "f1", "contentVersion": 1 }])
            );
            assert!(r.hub.holders_of(PID, "f2").is_empty(), "no phantom holder");
            assert_eq!(r.hub.holders_of(PID, "f1"), vec![me]);
        }

        /// P8: above 10 000 held frames the first chunk goes `full`, the rest
        /// as `add`.
        #[tokio::test]
        async fn over_ten_thousand_frames_chunks_full_then_add() {
            let r = rfx("send_receive").await;
            let present: Vec<(String, i32)> =
                (0..10_005).map(|i| (format!("u{i:05}"), 1)).collect();
            let from = request_count(&r.hub).await;
            assert_eq!(report_holders(&r.ctx, PID, &present).await.unwrap(), 10_005);
            let reqs = r.hub.server.received_requests().await.unwrap();
            let puts = holder_puts(&reqs[from..]);
            assert_eq!(puts.len(), 2);
            assert_eq!(puts[0]["full"], true);
            assert_eq!(puts[0]["add"].as_array().unwrap().len(), 10_000);
            assert_eq!(puts[1]["full"], false);
            assert_eq!(puts[1]["add"].as_array().unwrap().len(), 5);
            assert_eq!(puts[1]["add"][0]["frameUuid"], "u10000");
            assert_eq!(puts[1]["remove"], serde_json::json!([]));
        }

        // ── pass kinds and gates ─────────────────────────────────────────────

        /// Spec §5.5 / R15: a kicked pass does manifest-driven work only — no
        /// stat walk, no full holder report. The cadence pass does both.
        #[tokio::test]
        async fn a_kicked_pass_walks_no_disk_and_sends_no_full_report() {
            let r = rfx("send_receive").await;
            r.hub.seed_frames(PID, "acc-o", &["n1"], "published");
            sync_rows(&r).await;
            let path = land_file(&r, "f1", FrameOrigin::Replica, &pattern(1, 256));
            std::fs::remove_file(&path).unwrap();

            let rec = Arc::new(FetchRecorder::default());
            let from = request_count(&r.hub).await;
            let out = pass_with(&r.ctx, &rec, PassKind::Kicked, None, None).await;
            assert_eq!(out.reported, 0);
            assert!(row(&r.ctx, "f1").unwrap().on_disk, "no stat walk");
            let reqs = r.hub.server.received_requests().await.unwrap();
            assert!(holder_puts(&reqs[from..]).is_empty(), "no full report");
            assert_eq!(
                *rec.calls.lock().unwrap(),
                vec![(PID.to_string(), vec!["n1".to_string()])],
                "the need set still goes to the fetch"
            );

            let out = pass_with(&r.ctx, &rec, PassKind::Cadence, None, None).await;
            assert_eq!(out.reported, 1);
            assert!(!row(&r.ctx, "f1").unwrap().on_disk);
        }

        /// A `send` member replicates nothing (but still reports what it
        /// holds); a project with auto-replicate off is skipped by the timer
        /// pass and synced by a forced one.
        #[tokio::test]
        async fn the_role_gate_holds_and_only_a_forced_pass_overrides_the_toggle() {
            let send = rfx("send").await;
            send.hub.seed_frames(PID, "acc-o", &["n1"], "published");
            sync_rows(&send).await;
            let rec = Arc::new(FetchRecorder::default());
            for kind in [PassKind::Cadence, PassKind::Forced] {
                let out = pass_with(&send.ctx, &rec, kind, None, None).await;
                assert_eq!(out.reported, 1, "a send member still reports its holds");
            }
            assert!(rec.projects().is_empty(), "the role gate is never forced");

            let off = rfx("send_receive").await;
            off.hub.seed_frames(PID, "acc-o", &["n1"], "published");
            sync_rows(&off).await;
            set_project_auto_replicate(&off.ctx, PID, false).unwrap();
            let rec = Arc::new(FetchRecorder::default());
            pass_with(&off.ctx, &rec, PassKind::Cadence, None, None).await;
            pass_with(&off.ctx, &rec, PassKind::Kicked, None, None).await;
            assert!(rec.projects().is_empty(), "the toggle is off");
            let out = pass_with(&off.ctx, &rec, PassKind::Forced, Some(PID), None).await;
            assert_eq!(rec.projects(), vec![PID.to_string()]);
            assert_eq!(out.projects, 1);
            let out = pass_with(&off.ctx, &rec, PassKind::Forced, Some("other"), None).await;
            assert_eq!(out.projects, 0, "a scoped pass touches only its project");
        }

        /// Signed out, or no Collaboration root: the pass does nothing.
        #[tokio::test]
        async fn the_pass_is_a_no_op_signed_out_or_without_a_collaboration_root() {
            let Fx {
                tmp: _tmp,
                ctx,
                hub,
            } = fx("send_receive", false, false).await;
            crate::api::collab::refresh_projects(&ctx).await.unwrap();
            hub.seed_frames(PID, "acc-o", &["n1"], "published");
            sync_manifest(&ctx, PID, None, None).await.unwrap();
            let rec = Arc::new(FetchRecorder::default());
            let from = request_count(&hub).await;
            let out = pass_with(&ctx, &rec, PassKind::Cadence, None, None).await;
            assert_eq!(out, AutoSyncPassOutcome::default());
            assert!(rec.projects().is_empty());
            assert_eq!(
                request_count(&hub).await,
                from,
                "no hub call without a root"
            );

            let r = rfx("send_receive").await;
            {
                // A hub this device holds no token for = signed out.
                let conn = db(&r.ctx).unwrap().conn();
                crate::db::set_setting(
                    &conn,
                    crate::settings::keys::ACCOUNT_HUB_URL,
                    "http://signed-out.invalid",
                )
                .unwrap();
            }
            let out = pass_with(&r.ctx, &rec, PassKind::Cadence, None, None).await;
            assert_eq!(out, AutoSyncPassOutcome::default());
        }

        /// One project's failing fetch never ends the pass.
        #[tokio::test]
        async fn a_failing_fetch_does_not_end_the_pass() {
            let r = rfx("send_receive").await;
            r.hub.add_project(
                "p2",
                "m42",
                &[
                    ("acc-me", "send_receive", false),
                    ("acc-o", "send_receive", false),
                ],
                false,
            );
            crate::api::collab::refresh_projects(&r.ctx).await.unwrap();
            r.hub.seed_frames(PID, "acc-o", &["n1"], "published");
            r.hub.seed_frames("p2", "acc-o", &["n2"], "published");
            sync_rows(&r).await;
            sync_manifest(&r.ctx, "p2", None, None).await.unwrap();
            let rec = Arc::new(FetchRecorder::default());
            rec.fail.lock().unwrap().insert(PID.to_string());
            let out = pass_with(&r.ctx, &rec, PassKind::Cadence, None, None).await;
            let mut projects = rec.projects();
            projects.sort();
            assert_eq!(projects, vec![PID.to_string(), "p2".to_string()]);
            assert_eq!(out.failed, 1);
            assert_eq!(out.landed, 1);
        }

        // ── the policy commands ──────────────────────────────────────────────

        /// `set_collab_policy` stores the policy and returns what it selects;
        /// a preview stores nothing; a bad policy is refused.
        #[tokio::test]
        async fn set_policy_returns_a_preview_and_persists() {
            let r = rfx("send_receive").await;
            r.hub
                .seed_frames(PID, "acc-o", &["a", "b", "c"], "published");
            r.hub
                .update_frame(PID, "c", |f| f.filter_canonical = "Ha".into());
            sync_rows(&r).await;
            let held = land_file(&r, "a", FrameOrigin::Replica, &pattern(1, 1000));
            let _ = held;

            assert_eq!(
                get_collab_policy(&r.ctx, PID).await.unwrap(),
                ReplicationPolicy::default()
            );
            let l_only = ReplicationPolicy {
                filters: vec!["L".into()],
                ..Default::default()
            };
            let preview = set_collab_policy(&r.ctx, PID, l_only.clone())
                .await
                .unwrap();
            assert_eq!(
                preview,
                PolicyPreview {
                    frames: 2,
                    bytes: 2000,
                    already_held: 1,
                    to_fetch: 1,
                    to_fetch_bytes: 1000,
                }
            );
            assert_eq!(get_collab_policy(&r.ctx, PID).await.unwrap(), l_only);

            let ha = ReplicationPolicy {
                filters: vec!["Ha".into()],
                ..Default::default()
            };
            let p = preview_collab_policy(&r.ctx, PID, ha).await.unwrap();
            assert_eq!((p.frames, p.to_fetch), (1, 1));
            assert_eq!(
                get_collab_policy(&r.ctx, PID).await.unwrap(),
                l_only,
                "a preview stores nothing"
            );

            let bad = ReplicationPolicy {
                byte_budget: Some(-1),
                ..Default::default()
            };
            assert!(matches!(
                set_collab_policy(&r.ctx, PID, bad).await,
                Err(ApiError::Invalid(_))
            ));
            assert!(get_collab_policy(&r.ctx, "nope").await.is_err());
        }

        // ── real iroh: fetch, land, seed ─────────────────────────────────────

        /// P21 end to end: one pass fetches a 4 MiB frame from its only
        /// holder and lands it by RENAME — the store keeps only its outboard —
        /// under `<Collab>/<project>/<publisher>/`; the landed file is the
        /// seed, so a third device fetches it from the receiver alone.
        #[tokio::test]
        async fn frame_lands_by_rename_and_becomes_the_seed() {
            const SIZE: usize = 4 * 1024 * 1024;
            let r = rfx("send_receive").await;
            let recv = bind_receiver(&r).await;
            let pub_dir = tempfile::tempdir().unwrap();
            let publisher = peer_node(pub_dir.path()).await;
            pair(&recv, &publisher).await;
            let bytes = pattern(7, SIZE);
            let hash = publish_on(&r, &publisher, "f1", "c_x.fits", &bytes).await;
            sync_rows(&r).await;

            let out = real_pass(&r, PassKind::Cadence).await;
            assert_eq!((out.landed, out.failed), (1, 0), "{out:?}");

            let dest = r.collab.join("m31").join("other").join("c_x.fits");
            assert_eq!(std::fs::read(&dest).unwrap(), bytes, "identical bytes");
            let row = row(&r.ctx, "f1").unwrap();
            assert!(row.on_disk);
            assert_eq!(
                row.landed_path.as_deref(),
                Some(dest.to_string_lossy().as_ref())
            );
            let store_data = dir_bytes(&r.collab.join(".athenaeum").join("blobs").join("data"));
            assert!(
                store_data < (SIZE as u64) / 100,
                "the store's data was renamed out, not copied: {store_data} B left"
            );
            assert_eq!(collab_tags(&recv, "project/p1/f1/1").await, 1, "seeded");
            assert_eq!(collab_tags(&recv, "in-flight/").await, 0);
            assert!(
                r.hub
                    .holders_of(PID, "f1")
                    .contains(&B64.encode(own_node_for(&r.ctx))),
                "the landing is reported as a holder delta"
            );

            // A third device, with the publisher gone.
            let third_dir = tempfile::tempdir().unwrap();
            let third = peer_node(third_dir.path()).await;
            pair(&third, &recv).await;
            publisher.shutdown().await;
            let t_store = third.collab_store().unwrap();
            let results = tokio::time::timeout(
                Duration::from_secs(60),
                crate::sharing::iroh::blobs::fetch_blobs_assigned(
                    &t_store,
                    &third.endpoint(),
                    vec![crate::sharing::iroh::blobs::FrameFetch {
                        key: "f1".into(),
                        hash,
                        size: SIZE as u64,
                        providers: vec![iroh::EndpointId::from_bytes(&recv.node_id()).unwrap()],
                        in_flight_tag: "in-flight/project/p1/f1/1".into(),
                    }],
                    Arc::new(|_| {}),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(results[0].1.is_ok(), "{:?}", results[0].1);
            assert!(t_store.blobs().has(hash).await.unwrap());
            third.shutdown().await;
            recv.shutdown().await;
        }

        /// P20: a deleted landing is parked until GC — no fetch over the dead
        /// entry (iroh-blobs 0.103 would panic) — and fetched again once GC
        /// dropped the entry.
        #[tokio::test]
        async fn second_pass_after_delete_waits_for_gc_then_refetches() {
            use std::sync::atomic::{AtomicBool, Ordering};
            const SIZE: usize = 1024 * 1024;
            let r = rfx("send_receive").await;
            let gc_open = Arc::new(AtomicBool::new(false));
            crate::sharing::iroh::node::test_gc::arm(Some(Arc::clone(&gc_open)));
            let recv = bind_receiver(&r).await;
            crate::sharing::iroh::node::test_gc::arm(None);
            let pub_dir = tempfile::tempdir().unwrap();
            let publisher = peer_node(pub_dir.path()).await;
            pair(&recv, &publisher).await;
            let bytes = pattern(3, SIZE);
            let hash = publish_on(&r, &publisher, "f1", "c_x.fits", &bytes).await;
            sync_rows(&r).await;
            assert_eq!(real_pass(&r, PassKind::Cadence).await.landed, 1);

            let dest = PathBuf::from(row(&r.ctx, "f1").unwrap().landed_path.unwrap());
            std::fs::remove_file(&dest).unwrap();
            let before = sent_bytes(&publisher);
            let out = real_pass(&r, PassKind::Cadence).await;
            assert_eq!(out.attempted, 0, "{out:?}");
            let row1 = row(&r.ctx, "f1").unwrap();
            assert!(!row1.on_disk && row1.awaiting_gc);
            let served = sent_bytes(&publisher).saturating_sub(before);
            assert!(
                served < SERVED_PAYLOAD_FLOOR,
                "no request reached the provider: it sent {served} B"
            );

            gc_open.store(true, Ordering::SeqCst);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            while recv.collab_blob_health(hash).await.unwrap() != BlobHealth::Missing {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "GC never dropped the entry"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            gc_open.store(false, Ordering::SeqCst);

            let out = real_pass(&r, PassKind::Cadence).await;
            assert_eq!(out.landed, 1, "{out:?}");
            assert_eq!(std::fs::read(&dest).unwrap(), bytes, "the frame is back");
            assert!(row(&r.ctx, "f1").unwrap().on_disk);
            publisher.shutdown().await;
            recv.shutdown().await;
        }

        /// Plan step 7.5 (owner-approved): a new content version lands OVER
        /// the old file under the same name — never `c_x_2.fits` — and the
        /// old version's seed tag goes.
        #[tokio::test]
        async fn a_new_version_lands_over_the_old_file() {
            let r = rfx("send_receive").await;
            let recv = bind_receiver(&r).await;
            let pub_dir = tempfile::tempdir().unwrap();
            let publisher = peer_node(pub_dir.path()).await;
            pair(&recv, &publisher).await;
            let v1 = pattern(1, 256 * 1024);
            publish_on(&r, &publisher, "f1", "c_x.fits", &v1).await;
            sync_rows(&r).await;
            assert_eq!(real_pass(&r, PassKind::Cadence).await.landed, 1);
            let dest = r.collab.join("m31").join("other").join("c_x.fits");
            assert_eq!(std::fs::read(&dest).unwrap(), v1);

            // The publisher re-calibrates: version 2, new bytes, re-seeded and
            // reported as the v2 holder.
            let v2 = pattern(2, 256 * 1024);
            let v2_path = pub_dir.path().join("c_x_v2.fits");
            std::fs::write(&v2_path, &v2).unwrap();
            let h2 = publisher
                .seed_project_frame(PID, "f1", 2, &v2_path)
                .await
                .unwrap();
            let x2 = hash_bytes(&v2);
            r.hub.update_frame(PID, "f1", move |f| {
                f.content_version = 2;
                f.blake3 = h2.to_hex().to_string();
                f.xxh3 = x2;
            });
            crate::collab::hub_client::CollabClient::new(&r.hub.uri())
                .unwrap()
                .put_holders(
                    "tok-o",
                    PID,
                    false,
                    &[crate::collab::hub_client::HolderRefWire {
                        frame_uuid: "f1".into(),
                        content_version: 2,
                    }],
                    &[],
                )
                .await
                .unwrap();
            sync_rows(&r).await;
            assert!(
                !row(&r.ctx, "f1").unwrap().on_disk,
                "a new version is needed"
            );

            let out = real_pass(&r, PassKind::Kicked).await;
            assert_eq!(out.landed, 1, "{out:?}");
            assert_eq!(std::fs::read(&dest).unwrap(), v2, "v2 over v1, same path");
            assert!(!r
                .collab
                .join("m31")
                .join("other")
                .join("c_x_2.fits")
                .exists());
            assert_eq!(
                collab_tags(&recv, "project/p1/f1/1").await,
                0,
                "v1 unseeded"
            );
            assert_eq!(collab_tags(&recv, "project/p1/f1/2").await, 1);
            let row = row(&r.ctx, "f1").unwrap();
            assert!(row.on_disk);
            assert_eq!(row.content_version, 2);
            publisher.shutdown().await;
            recv.shutdown().await;
        }

        /// P11 isolation: a frame no holder can serve fails alone; its sibling
        /// lands, and the failed frame's in-flight tag is gone.
        #[tokio::test]
        async fn fetch_failure_of_one_frame_lands_the_others() {
            let r = rfx("send_receive").await;
            let recv = bind_receiver(&r).await;
            let pub_dir = tempfile::tempdir().unwrap();
            let publisher = peer_node(pub_dir.path()).await;
            pair(&recv, &publisher).await;
            let good = pattern(1, 128 * 1024);
            publish_on(&r, &publisher, "good", "good.fits", &good).await;
            // Listed with the publisher as holder, but never seeded there.
            let ghost = pattern(2, 128 * 1024);
            r.hub.seed_frames(PID, "acc-o", &["ghost"], "published");
            let (gh, gx) = (
                blake3::hash(&ghost).to_hex().to_string(),
                hash_bytes(&ghost),
            );
            r.hub.update_frame(PID, "ghost", move |f| {
                f.blake3 = gh;
                f.xxh3 = gx;
                f.byte_size = 128 * 1024;
            });
            sync_rows(&r).await;

            let out = real_pass(&r, PassKind::Cadence).await;
            assert_eq!((out.landed, out.failed), (1, 1), "{out:?}");
            assert!(row(&r.ctx, "good").unwrap().on_disk);
            let ghost_row = row(&r.ctx, "ghost").unwrap();
            assert!(!ghost_row.on_disk);
            assert!(ghost_row.last_error.is_some());
            assert_eq!(collab_tags(&recv, "in-flight/").await, 0);
            publisher.shutdown().await;
            recv.shutdown().await;
        }

        /// P24: a second frame with the content of one already landed is
        /// linked from that file — no bytes move — and seeded.
        #[tokio::test]
        async fn identical_content_second_frame_is_linked_not_fetched() {
            const SIZE: usize = 1024 * 1024;
            let r = rfx("send_receive").await;
            let recv = bind_receiver(&r).await;
            let pub_dir = tempfile::tempdir().unwrap();
            let publisher = peer_node(pub_dir.path()).await;
            pair(&recv, &publisher).await;
            let bytes = pattern(5, SIZE);
            let hash = publish_on(&r, &publisher, "f1", "c_x.fits", &bytes).await;
            sync_rows(&r).await;
            assert_eq!(real_pass(&r, PassKind::Cadence).await.landed, 1);

            r.hub.seed_frames(PID, "acc-o", &["f2"], "published");
            let x = hash_bytes(&bytes);
            r.hub.update_frame(PID, "f2", move |f| {
                f.file_name = "c_y.fits".into();
                f.blake3 = hash.to_hex().to_string();
                f.xxh3 = x;
                f.byte_size = SIZE as i64;
            });
            sync_rows(&r).await;
            let before = sent_bytes(&publisher);
            let out = real_pass(&r, PassKind::Kicked).await;
            assert_eq!(out.landed, 1, "{out:?}");
            let dest = r.collab.join("m31").join("other").join("c_y.fits");
            assert_eq!(std::fs::read(&dest).unwrap(), bytes);
            assert!(row(&r.ctx, "f2").unwrap().on_disk);
            assert_eq!(collab_tags(&recv, "project/p1/f2/1").await, 1);
            let served = sent_bytes(&publisher).saturating_sub(before);
            assert!(
                served < SERVED_PAYLOAD_FLOOR,
                "linked, not fetched: {served} B"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let first = r.collab.join("m31").join("other").join("c_x.fits");
                assert_eq!(
                    std::fs::metadata(&dest).unwrap().ino(),
                    std::fs::metadata(&first).unwrap().ino(),
                    "P24 links the landed file (same volume), never a second copy"
                );
            }

            // Two identical frames needed in ONE batch: one fetch, two landings.
            let twin = pattern(6, SIZE);
            let twin_hash = publish_on(&r, &publisher, "t1", "t_1.fits", &twin).await;
            r.hub.seed_frames(PID, "acc-o", &["t2"], "published");
            let x = hash_bytes(&twin);
            r.hub.update_frame(PID, "t2", move |f| {
                f.file_name = "t_2.fits".into();
                f.blake3 = twin_hash.to_hex().to_string();
                f.xxh3 = x;
                f.byte_size = SIZE as i64;
            });
            sync_rows(&r).await;
            let before = sent_bytes(&publisher);
            let out = real_pass(&r, PassKind::Kicked).await;
            assert_eq!((out.landed, out.failed), (2, 0), "{out:?}");
            for name in ["t_1.fits", "t_2.fits"] {
                let p = r.collab.join("m31").join("other").join(name);
                assert_eq!(std::fs::read(&p).unwrap(), twin, "{name}");
            }
            let served = sent_bytes(&publisher).saturating_sub(before);
            assert!(
                served < (SIZE as u64) * 3 / 2,
                "the content moved once for both frames: {served} B"
            );
            publisher.shutdown().await;
            recv.shutdown().await;
        }
    }
}
