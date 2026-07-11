//! App-shell retention (Stage I, task M4) — the capture-node counterpart of
//! Perseus's retention loop, wired through the app's catalog.
//!
//! This module reclaims local disk on a **full-app capture node** once its frames
//! are confirmed on the paired primary. It reuses the safety-critical decision
//! core in [`crate::sync::retention`] verbatim — the same
//! [`evaluate_and_apply`](crate::sync::evaluate_and_apply), the same single
//! chokepoint ([`SyncStore::confirmed`](crate::sync::SyncStore::confirmed)), the
//! same binding deleter contract — and only supplies the two app-specific pieces
//! Perseus supplies for itself:
//!
//! 1. **A source↔package linkage.** Perseus maps a confirmed package back to its
//!    capture file via `perseus_seen`. The app builds its packages from *catalog*
//!    files, so it records the equivalent linkage in the catalog's `sync_sources`
//!    table at package-build time (see [`crate::api::sync`]'s
//!    `build_selection_package`). One app package carries many files, so this is
//!    a one-package → many-sources join.
//! 2. **A catalog-consistent deleter.** Where Perseus does a plain `remove_file`,
//!    the app must also keep its catalog honest: deleting a synced source removes
//!    the disk file AND its `files` row (CASCADE → `frames`, tags, calibration
//!    links, sessions). This routes through the same catalog-consistent delete
//!    primitive the app's Black-Hole "send to void" uses
//!    ([`crate::db::send_to_void`]).
//!
//! # The hard invariant is unchanged
//!
//! Only **confirmed** packages are ever eligible (enforced in exactly one place,
//! upstream in [`crate::sync::retention`]); an unconfirmed / in-flight / failed
//! package can never reach this deleter, at any disk pressure. And dry-run is the
//! default: live deletion needs BOTH `sync.retention.dry_run = "false"` AND the
//! explicit `sync.retention.live_confirmed = "true"` opt-in — the app equivalent
//! of Perseus's `i_have_verified_the_soak`. Either missing forces dry-run
//! ([`AppRetentionConfig::effective_dry_run`]). A fresh install defaults to
//! `keep_everything` + dry-run + no opt-in, so it never deletes anything.
//!
//! # The deleter contract (binding — see [`crate::sync::retention`] module docs)
//!
//! [`app_retention_delete_package`] honours all three clauses:
//! 1. **Audit before delete.** A `sync_history` `retention_deleted` row is
//!    committed *before* any file/row is removed; if that persist fails, the
//!    delete is refused (`Err`) and the source survives.
//! 2. **Honest [`DeleteOutcome`].** Only a real removal reports `Removed`; every
//!    legitimate no-op (no live linkage, already gone out-of-band, stat-guard or
//!    catalog-drift decline) reports `SkippedNoop` and is never counted.
//! 3. **Re-verify before removal (TOCTOU guard).** Each source is re-stat'd and
//!    required to match the `(size, mtime)` recorded at build time immediately
//!    before removal; a since-rewritten path is skipped, not destroyed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use crate::api::{db, ApiError};
use crate::services::ServiceContext;
use crate::settings::{defaults, keys};
use crate::sync::store::insert_history_row;
use crate::sync::{
    evaluate_and_apply, live_sources_for_package, mark_sync_source_deleted, CatalogSyncStore,
    DeleteOutcome, Direction, HistoryRow, RetentionOutcome, RetentionPolicy, SyncSourceRow,
};

/// Retention evaluation cadence — hourly, matching Perseus's default.
pub const DEFAULT_RETENTION_INTERVAL_SECS: u64 = 3600;

/// Milliseconds-since-epoch rendering of a [`SystemTime`], `0` when unavailable.
///
/// The single source of truth for the retention stat comparison: the mtime
/// recorded into `sync_sources` at build time and the mtime re-read by the
/// TOCTOU guard MUST be computed identically, or the guard would spuriously
/// reject everything. `build_selection_package` and [`delete_one_source`] both
/// route through here.
pub fn mtime_millis(t: Option<SystemTime>) -> i64 {
    t.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// RFC3339 UTC millisecond timestamp — the sync tables' canonical rendering.
fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Current disk usage of the volume holding `path`, as a whole percent
/// (`0..=100`). Only [`RetentionPolicy::DiskPct`] consults it.
///
/// Fails **safe**: any error, or a platform without `statvfs`, returns `0`
/// ("empty disk") so a bad reading can never *trigger* a deletion — it can only
/// decline to. Mirrors Perseus's probe.
#[cfg(unix)]
pub fn disk_usage_pct(path: &Path) -> u8 {
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
        tracing::warn!(path = %path.display(), "statvfs failed; retention disk probe returns 0%");
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
pub fn disk_usage_pct(_path: &Path) -> u8 {
    // No statvfs; treat as empty so retention never deletes on disk pressure.
    0
}

/// The retention configuration for a full-app capture node, read from settings.
///
/// `effective_dry_run` is where the app's soak opt-in is enforced: live deletion
/// is honoured ONLY when the raw `dry_run` is explicitly `false` AND the explicit
/// `live_confirmed` opt-in is `true`. This is deliberately a **read-time** gate,
/// not merely a set-time validation: it makes live deletion structurally
/// impossible unless both flags line up, no matter how the settings were written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRetentionConfig {
    pub policy: RetentionPolicy,
    /// The raw `sync.retention.dry_run` value (default `true`).
    pub raw_dry_run: bool,
    /// The raw `sync.retention.live_confirmed` opt-in (default `false`).
    pub live_confirmed: bool,
}

/// Parse the raw `sync.retention.dry_run` setting fail-safe: dry-run stays ON
/// unless the value is *explicitly* `"false"` (case-insensitive). Any other
/// string — empty, garbage, a typo like `"flase"`, `"0"` — leans to dry-run
/// (`true`), never toward live deletion. Pairs with the `live_confirmed` opt-in
/// gate in [`AppRetentionConfig::effective_dry_run`].
fn parse_dry_run(value: &str) -> bool {
    !value.eq_ignore_ascii_case("false")
}

impl AppRetentionConfig {
    /// The dry-run mode actually applied. Live deletion (returns `false`) only
    /// when the operator has BOTH disabled dry-run AND set the explicit opt-in;
    /// any other combination forces dry-run (`true`), the safe default.
    pub fn effective_dry_run(&self) -> bool {
        !(!self.raw_dry_run && self.live_confirmed)
    }

    /// Load from settings (runtime > DB > default). Unknown policy strings and
    /// unparseable tuning values fall back to the SAFE default (`keep_everything`
    /// / the documented tuning defaults).
    pub fn load(ctx: &ServiceContext) -> Result<Self, ApiError> {
        let db = db(ctx)?;
        let conn = db.conn();
        let get = |key: &str, default: &str| -> Result<String, ApiError> {
            Ok(ctx.settings.get_with_precedence(&conn, key, default)?)
        };
        let policy_str = get(keys::SYNC_RETENTION_POLICY, defaults::SYNC_RETENTION_POLICY)?;
        let keep_days = get(keys::SYNC_RETENTION_KEEP_DAYS, defaults::SYNC_RETENTION_KEEP_DAYS)?
            .parse::<u32>()
            .ok()
            .filter(|d| *d >= 1)
            .unwrap_or_else(|| defaults::SYNC_RETENTION_KEEP_DAYS.parse().unwrap_or(30));
        let disk_max_pct = get(keys::SYNC_RETENTION_DISK_MAX_PCT, defaults::SYNC_RETENTION_DISK_MAX_PCT)?
            .parse::<u8>()
            .ok()
            .filter(|p| (1..=100).contains(p))
            .unwrap_or_else(|| defaults::SYNC_RETENTION_DISK_MAX_PCT.parse().unwrap_or(90));
        let raw_dry_run =
            parse_dry_run(&get(keys::SYNC_RETENTION_DRY_RUN, defaults::SYNC_RETENTION_DRY_RUN)?);
        let live_confirmed =
            get(keys::SYNC_RETENTION_LIVE_CONFIRMED, defaults::SYNC_RETENTION_LIVE_CONFIRMED)?
                .eq_ignore_ascii_case("true");

        let policy = match policy_str.as_str() {
            "on_confirm" => RetentionPolicy::OnConfirm,
            "keep_days" => RetentionPolicy::KeepDays(keep_days),
            "disk_pct" => RetentionPolicy::DiskPct { max_pct: disk_max_pct },
            // "keep_everything" and any unknown value → the safe default.
            _ => RetentionPolicy::KeepEverything,
        };
        Ok(Self { policy, raw_dry_run, live_confirmed })
    }
}

/// Whether the retention tick should run at all: this device is signed in.
/// Network-free — sign-in from the local token store (never the hub). In the
/// mesh model (Sync 2C) every signed-in node is a full peer that can send, so
/// sender-copy retention applies to any of them; the old `role == capture` gate
/// is gone. A signed-out device is never ready, so it never opens a store,
/// resolves confirmed rows, or considers a delete.
pub fn retention_ready(ctx: &ServiceContext) -> Result<bool, ApiError> {
    Ok(crate::api::account::hub_credentials(ctx)?.is_some())
}

/// The volume retention measures for the `disk_pct` policy: the first enabled
/// scan root (where light frames live), else the catalog directory. Best-effort
/// — an unresolvable path yields `"."`, whose probe fails safe to `0%`.
fn retention_probe_path(ctx: &ServiceContext) -> PathBuf {
    if let Ok(db) = db(ctx) {
        {
            let conn = db.conn();
            if let Ok(roots) = crate::db::get_scan_roots(&conn) {
                if let Some(r) = roots.iter().find(|r| r.enabled) {
                    return PathBuf::from(&r.path);
                }
            }
        }
        if let Some(parent) = db.path().parent() {
            return parent.to_path_buf();
        }
    }
    PathBuf::from(".")
}

/// Build the `retention_deleted` audit row for one source. Reads frame identity
/// (uuid/object/filename) and the peer from the still-present catalog / outbound
/// rows; falls back to the path basename when the catalog row is already gone, so
/// there is always a persistable row (a degraded audit beats none).
fn build_source_history_row(conn: &Connection, package_ref: &str, src: &SyncSourceRow) -> HistoryRow {
    let (filename, frame_uuid, object) = src
        .file_id
        .and_then(|fid| {
            conn.query_row(
                "SELECT f.filename, fr.uuid, fr.object
                 FROM files f LEFT JOIN frames fr ON fr.file_id = f.id
                 WHERE f.id = ?1",
                params![fid],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()
            .ok()
            .flatten()
        })
        .map(|(fname, uuid, obj)| (fname, uuid.unwrap_or_default(), obj))
        .unwrap_or_else(|| {
            let fname = Path::new(&src.path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            (fname, String::new(), None)
        });

    let peer_device = conn
        .query_row(
            "SELECT peer FROM sync_outbound WHERE package_ref = ?1 ORDER BY id DESC LIMIT 1",
            params![package_ref],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or_default();

    let now = now_iso();
    HistoryRow {
        frame_uuid,
        filename,
        object,
        peer_device,
        direction: Direction::Sent,
        bytes: src.size,
        started_at: now.clone(),
        finished_at: Some(now),
        outcome: "retention_deleted".to_string(),
    }
}

/// Delete one confirmed source, catalog-consistently. Returns `Ok(true)` when the
/// file was actually removed, `Ok(false)` for a legitimate no-op, `Err` only when
/// the audit could not be persisted (the delete is then refused, file untouched)
/// or a destructive step failed.
///
/// The ordering is the deleter contract made concrete: linkage/stat/catalog
/// checks first (any decline returns `Ok(false)` before writing anything), then a
/// committed audit row, then the destructive removal, then the linkage stamp.
fn delete_one_source(conn: &Connection, package_ref: &str, src: &SyncSourceRow) -> Result<bool> {
    let path = Path::new(&src.path);

    // Out-of-band already gone: nothing to reclaim. Stamp the linkage dead so it
    // stops being offered; do NOT touch any catalog row (missing files are not
    // orphans — repo convention) and write no `retention_deleted` audit row.
    if !path.exists() {
        tracing::info!(
            package_ref,
            path = %src.path,
            outcome = "retention_source_missing",
            "retention: app source already gone out-of-band; marking linkage dead"
        );
        mark_sync_source_deleted(conn, package_ref, &src.path)?;
        return Ok(false);
    }

    // TOCTOU stat guard: re-stat and require an exact match against the stat
    // recorded at build time. A mismatch means the path was rewritten with new,
    // never-synced content since confirmation — deleting it would destroy live
    // data, so skip.
    let meta = std::fs::metadata(path)
        .with_context(|| format!("stat source before retention delete {}", src.path))?;
    if meta.len() != src.size || mtime_millis(meta.modified().ok()) != src.mtime_ms {
        tracing::warn!(
            package_ref,
            path = %src.path,
            "retention skip: app source changed since confirmation"
        );
        return Ok(false);
    }

    // Catalog-consistency gate. Decide BEFORE any destructive action or audit:
    //   - row present at THIS exact path → full catalog-consistent delete
    //   - row moved to a different path  → skip (catalog now points elsewhere)
    //   - no file_id / row already gone  → reclaim the disk file only
    let catalog_path: Option<String> = match src.file_id {
        Some(fid) => conn
            .query_row("SELECT path FROM files WHERE id = ?1", params![fid], |r| r.get(0))
            .optional()
            .context("resolve current catalog path for retention")?,
        None => None,
    };
    let full_catalog_delete = match (src.file_id, &catalog_path) {
        (Some(_), Some(p)) if p == &src.path => true,
        (Some(_), Some(_)) => {
            tracing::warn!(
                package_ref,
                path = %src.path,
                "retention skip: catalog path drifted since confirmation"
            );
            return Ok(false);
        }
        // No file_id, or the catalog no longer tracks it: reclaim disk only.
        _ => false,
    };

    // ── Audit BEFORE the destructive action ──────────────────────────────────
    // Committed here (autocommit); if it fails, the `?` propagates and NOTHING is
    // removed — "file gone, audit missing" is the one forbidden outcome.
    let hist = build_source_history_row(conn, package_ref, src);
    insert_history_row(conn, &hist)
        .with_context(|| format!("persist retention audit for {}", src.path))?;

    // ── Destructive action ───────────────────────────────────────────────────
    if full_catalog_delete {
        // Reuse the app's proven catalog-consistent delete: removes the file from
        // disk AND the `files` row (CASCADE → frames, tags, calibration links,
        // sessions), plus any Black-Hole row. Runs after the audit is durable.
        crate::db::send_to_void(conn, src.file_id.expect("full delete implies file_id"))
            .with_context(|| format!("catalog-consistent retention delete {}", src.path))?;
    } else {
        // No catalog row to drop — just reclaim the disk file.
        std::fs::remove_file(path)
            .with_context(|| format!("retention delete app source {}", src.path))?;
    }

    // Stamp the linkage dead so it never surfaces again. Best-effort: a failure
    // here leaves the row "live", but the file is gone, so the next pass hits the
    // out-of-band branch and stamps it — self-healing, never a re-delete.
    if let Err(error) = mark_sync_source_deleted(conn, package_ref, &src.path) {
        tracing::error!(
            package_ref,
            path = %src.path,
            %error,
            "retention: failed to stamp sync_sources row deleted after a successful delete"
        );
    }

    tracing::info!(
        package_ref,
        path = %src.path,
        outcome = "retention_deleted",
        "retention deleted app source"
    );
    Ok(true)
}

/// The app-shell retention deleter for one confirmed package. `package_ref` is
/// the `sync_outbound.package_ref` core resolved as eligible; this maps it back to
/// its `sync_sources` linkage and deletes each live source catalog-consistently.
///
/// Returns [`DeleteOutcome::Removed`] when at least one source was actually
/// removed, [`DeleteOutcome::SkippedNoop`] when the package had no live linkage or
/// every source was a legitimate no-op. An audit-persist failure on any source
/// propagates as `Err` (that source, and the pass, err toward keeping data).
///
/// All work runs on the store's single locked connection — [`SyncStore::confirmed`]
/// has already released it before the deleter is called, so there is no re-entrant
/// lock.
fn app_retention_delete_package(store: &CatalogSyncStore, package_ref: &Path) -> Result<DeleteOutcome> {
    let conn = store.lock_conn();
    let pkg_ref = package_ref.to_string_lossy();

    let sources = live_sources_for_package(&conn, &pkg_ref)?;
    if sources.is_empty() {
        tracing::debug!(
            package_ref = %package_ref.display(),
            "retention: no live source linkage for confirmed package; skipping (already handled or never linked)"
        );
        return Ok(DeleteOutcome::SkippedNoop);
    }

    let mut removed_any = false;
    for src in &sources {
        if delete_one_source(&conn, &pkg_ref, src)? {
            removed_any = true;
        }
    }
    Ok(if removed_any {
        DeleteOutcome::Removed
    } else {
        DeleteOutcome::SkippedNoop
    })
}

/// Evaluate `cfg` over the store's confirmed packages and (unless dry-run) apply
/// it through the catalog-consistent deleter. The pure seam the tests drive: the
/// candidate source is the store's `confirmed()`, the destructive seam is
/// [`app_retention_delete_package`], and the dry-run decision is
/// [`AppRetentionConfig::effective_dry_run`] — so the soak opt-in is honoured here
/// too, not only in the loop.
pub fn evaluate(
    store: &CatalogSyncStore,
    cfg: &AppRetentionConfig,
    now: DateTime<Utc>,
    disk_probe: &dyn Fn() -> u8,
) -> Result<RetentionOutcome> {
    let dry_run = cfg.effective_dry_run();
    let mut deleter = |pkg: &Path| app_retention_delete_package(store, pkg);
    evaluate_and_apply(store, &cfg.policy, dry_run, now, disk_probe, &mut deleter)
}

/// One retention pass for a full-app node. `Ok(None)` when the device is not
/// ready (not signed in) or the policy is `keep_everything` (nothing to do) —
/// neither opens a store nor touches disk. Otherwise opens the catalog-backed
/// store and runs [`evaluate`].
///
/// `now` + `disk_probe` are injected so the hourly loop can pass the live clock /
/// statvfs probe while tests pass deterministic ones.
pub fn run_app_retention_once(
    ctx: &ServiceContext,
    now: DateTime<Utc>,
    disk_probe: &dyn Fn() -> u8,
) -> Result<Option<RetentionOutcome>, ApiError> {
    if !retention_ready(ctx)? {
        return Ok(None);
    }
    let cfg = AppRetentionConfig::load(ctx)?;
    if matches!(cfg.policy, RetentionPolicy::KeepEverything) {
        tracing::debug!("retention: keep_everything — nothing to do this tick");
        return Ok(None);
    }
    // Visible each tick: dry_run disabled without the opt-in means we are STILL
    // dry-running (the safe default) — surface it so a half-configured go-live is
    // never silent.
    if !cfg.raw_dry_run && !cfg.live_confirmed {
        tracing::warn!(
            policy = cfg.policy.label(),
            "retention: dry_run=false ignored — sync.retention.live_confirmed opt-in is not set; staying in dry-run"
        );
    }

    let db_path = {
        let db = db(ctx)?;
        db.path().to_path_buf()
    };
    let store = CatalogSyncStore::open(&db_path)
        .map_err(|e| ApiError::Internal(format!("open catalog sync store for retention: {e:#}")))?;

    let outcome = evaluate(&store, &cfg, now, disk_probe)
        .map_err(|e| ApiError::Internal(format!("retention evaluate: {e:#}")))?;

    tracing::info!(
        policy = cfg.policy.label(),
        dry_run = outcome.dry_run,
        live_confirmed = cfg.live_confirmed,
        eligible = outcome.eligible.len(),
        deleted = outcome.deleted.len(),
        would_warn_disk_pressure = outcome.would_warn_disk_pressure,
        "app retention tick complete"
    );
    Ok(Some(outcome))
}

/// The hourly (config-cadence) retention loop for a full-app node. Each tick
/// runs a full pass on a blocking thread (SQLite + fs). Gated inside
/// [`run_app_retention_once`], so it is a cheap no-op on any signed-out /
/// keep-everything device — safe to spawn unconditionally at host start. Never
/// fails the host; every error is logged and the loop continues.
pub async fn run_app_retention_loop(ctx: Arc<ServiceContext>, interval: Duration) {
    tracing::info!(interval_secs = interval.as_secs(), "app retention loop armed");
    loop {
        tokio::time::sleep(interval).await;
        let ctx = Arc::clone(&ctx);
        let res = tokio::task::spawn_blocking(move || {
            let probe_path = retention_probe_path(&ctx);
            let disk_probe = move || disk_usage_pct(&probe_path);
            run_app_retention_once(&ctx, Utc::now(), &disk_probe)
        })
        .await;
        match res {
            Ok(Ok(Some(_outcome))) => {}
            Ok(Ok(None)) => tracing::debug!("retention tick skipped (signed out or keep_everything)"),
            Ok(Err(error)) => tracing::error!(error = %error, "retention tick failed"),
            Err(error) => tracing::error!(%error, "retention tick task panicked"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::sharing::types::NodeId;
    use crate::sync::store::insert_history_row;
    use crate::sync::{HistoryQuery, SyncStore};
    use std::cell::Cell;

    const PEER: NodeId = [5u8; 32];

    /// A minimal real-`Database` [`ServiceContext`] (tempdir SQLite), mirroring
    /// the construction pattern in `api::sync`/`api::account` tests. Used by the
    /// `retention_ready` gate test, which needs the full context (settings +
    /// token store) rather than a bare `Connection`.
    fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
        use crate::cache::MemoryImageCache;
        use crate::services::compute_queue::ComputeQueue;
        use crate::services::operation_queue::OperationQueue;
        use crate::settings::SettingsManager;
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};
        #[cfg(all(feature = "render", feature = "solver"))]
        use std::sync::RwLock;

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
            active_registrations: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            active_light_cal: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(all(feature = "render", feature = "solver"))]
            dso_catalog: Arc::new(RwLock::new(None)),
            #[cfg(feature = "solver")]
            star_cache: Arc::new(RwLock::new(None)),
            #[cfg(feature = "solver")]
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
        };
        (tmp, ctx)
    }

    /// Sign this node in by writing a device token through the real account path
    /// (so `hub_credentials` resolves it). No role/peer is set — the mesh model
    /// (Sync 2C) has no role, and `retention_ready` must key on signed-in alone.
    fn set_hub_credentials(ctx: &ServiceContext) {
        crate::api::account::store_token_for_test(ctx, "test-device-token").unwrap();
    }

    /// Sync 2C: sender-copy retention runs on ANY signed-in node — the old
    /// `role == Capture` gate is gone (the mesh model has no role). Signed out →
    /// not ready; signed in (no role) → ready. RED against the pre-2C gate, which
    /// required a persisted `capture` role that this test never sets.
    #[test]
    fn retention_ready_is_signed_in_not_role() {
        let (_tmp, ctx) = test_ctx();
        // signed out → not ready
        assert!(!retention_ready(&ctx).unwrap());
        set_hub_credentials(&ctx);
        assert!(
            retention_ready(&ctx).unwrap(),
            "any signed-in node runs sender-copy retention"
        );
    }

    /// A confirmed app package with one linked source on disk. Returns the temp
    /// dir (kept alive), the catalog db path, the frame's source path, its
    /// `file_id`, and the `package_ref`.
    struct Fixture {
        _tmp: tempfile::TempDir,
        db_path: PathBuf,
        src: PathBuf,
        file_id: i64,
        package_ref: String,
    }

    /// Insert a `files` + `frames` row for a real on-disk file and return
    /// `(file_id, size, mtime_ms)`.
    fn insert_catalog_file(conn: &Connection, path: &Path) -> (i64, u64, i64) {
        let meta = std::fs::metadata(path).unwrap();
        let size = meta.len();
        let mtime_ms = mtime_millis(meta.modified().ok());
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at, uuid)
             VALUES (?1, ?2, ?3, '2026-07-06T00:00:00Z', 'FITS', '2026-07-06T00:00:00Z', ?4)",
            params![
                path.to_string_lossy(),
                path.file_name().unwrap().to_str().unwrap(),
                size as i64,
                uuid::Uuid::new_v4().to_string(),
            ],
        )
        .unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, object, uuid) VALUES (?1, 'M42', ?2)",
            params![file_id, uuid::Uuid::new_v4().to_string()],
        )
        .unwrap();
        (file_id, size, mtime_ms)
    }

    /// Build a confirmed-package fixture: one file+frame on disk, a confirmed
    /// `sync_outbound` row, and its `sync_sources` linkage. `confirm` controls
    /// whether the outbound row is confirmed (unconfirmed exercises the invariant).
    fn make_fixture(confirm: bool) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let src = tmp.path().join("light-0001.fits");
        std::fs::write(&src, b"frame-bytes").unwrap();

        let conn = Connection::open(&db_path).unwrap();
        init_db(&conn).unwrap();
        let (file_id, size, mtime_ms) = insert_catalog_file(&conn, &src);

        let package_ref = tmp.path().join("packages/pkg-1").to_string_lossy().to_string();
        conn.execute(
            "INSERT INTO sync_outbound (package_ref, peer, state, attempts, created_at, confirmed_at)
             VALUES (?1, ?2, ?3, 0, '2026-07-06T00:00:00.000Z', ?4)",
            params![
                package_ref,
                crate::sync::node_id_hex(&PEER),
                if confirm { "confirmed" } else { "queued" },
                if confirm { Some("2026-07-06T00:00:00.000Z") } else { None },
            ],
        )
        .unwrap();
        crate::sync::insert_sync_source(&conn, &package_ref, Some(file_id), &src.to_string_lossy(), size, mtime_ms)
            .unwrap();

        Fixture { _tmp: tmp, db_path, src, file_id, package_ref }
    }

    fn cfg(policy: RetentionPolicy, dry_run: bool, live: bool) -> AppRetentionConfig {
        AppRetentionConfig { policy, raw_dry_run: dry_run, live_confirmed: live }
    }

    fn history_outcome_count(store: &CatalogSyncStore, outcome: &str) -> usize {
        store
            .search_history(HistoryQuery { filename: None, object: None, direction: None, peer: None, limit: 1000 })
            .unwrap()
            .iter()
            .filter(|h| h.outcome == outcome)
            .count()
    }

    fn frame_count(store: &CatalogSyncStore, file_id: i64) -> i64 {
        let conn = store.lock_conn();
        conn.query_row("SELECT COUNT(*) FROM frames WHERE file_id = ?1", params![file_id], |r| r.get(0))
            .unwrap()
    }

    fn file_count(store: &CatalogSyncStore, file_id: i64) -> i64 {
        let conn = store.lock_conn();
        conn.query_row("SELECT COUNT(*) FROM files WHERE id = ?1", params![file_id], |r| r.get(0))
            .unwrap()
    }

    // ── effective dry-run (the app soak opt-in) ──────────────────────────────

    /// Live deletion is enabled ONLY when dry_run=false AND live_confirmed=true;
    /// every other combination stays dry-run. The safety net that makes the
    /// hard invariant hold no matter how settings are written.
    #[test]
    fn effective_dry_run_requires_both_flags() {
        assert!(cfg(RetentionPolicy::OnConfirm, true, true).effective_dry_run(), "dry_run=true wins");
        assert!(cfg(RetentionPolicy::OnConfirm, true, false).effective_dry_run(), "default");
        assert!(
            cfg(RetentionPolicy::OnConfirm, false, false).effective_dry_run(),
            "dry_run=false without the opt-in must STILL be dry-run"
        );
        assert!(
            !cfg(RetentionPolicy::OnConfirm, false, true).effective_dry_run(),
            "only dry_run=false + opt-in goes live"
        );
    }

    /// The raw `dry_run` setting parses fail-safe: ONLY an explicit `"false"`
    /// (any case) disables dry-run. Every other value — empty, garbage, a typo,
    /// `"0"` — must resolve to dry-run (`true`) so a corrupt/unexpected setting
    /// never leans toward live deletion.
    #[test]
    fn dry_run_parse_defaults_to_dry_on_garbage() {
        // Only explicit "false" (case-insensitive) disables dry-run.
        assert!(!parse_dry_run("false"), "explicit false disables dry-run");
        assert!(!parse_dry_run("FALSE"), "case-insensitive false");
        assert!(!parse_dry_run("False"), "mixed-case false");
        // Everything else stays dry-run.
        assert!(parse_dry_run("true"), "true is dry-run");
        assert!(parse_dry_run(""), "empty stays dry-run");
        assert!(parse_dry_run("garbage"), "garbage stays dry-run");
        assert!(parse_dry_run("flase"), "a typo of false stays dry-run");
        assert!(parse_dry_run("0"), "a numeric 0 stays dry-run");
        assert!(parse_dry_run(" false "), "untrimmed 'false' stays dry-run (not an exact match)");
    }

    // ── BRD B6: catalog-consistent delete + two searchable events ────────────

    /// Live mode: a confirmed source is deleted from disk AND its catalog rows
    /// (`files` + CASCADE `frames`) are gone, and BOTH the transfer event and the
    /// retention-deletion event are searchable in history (BRD B6 acceptance).
    #[test]
    fn live_delete_removes_file_and_catalog_rows_with_two_history_events() {
        let fx = make_fixture(true);
        let store = CatalogSyncStore::open(&fx.db_path).unwrap();
        // Represent the transfer that preceded retention: a "sent" history row.
        {
            let conn = store.lock_conn();
            insert_history_row(
                &conn,
                &HistoryRow {
                    frame_uuid: "u1".into(),
                    filename: "light-0001.fits".into(),
                    object: Some("M42".into()),
                    peer_device: crate::sync::node_id_hex(&PEER),
                    direction: Direction::Sent,
                    bytes: 11,
                    started_at: "2026-07-06T00:00:00.000Z".into(),
                    finished_at: Some("2026-07-06T00:00:01.000Z".into()),
                    outcome: "sent".into(),
                },
            )
            .unwrap();
        }

        let outcome = evaluate(&store, &cfg(RetentionPolicy::OnConfirm, false, true), Utc::now(), &|| 0u8)
            .unwrap();

        assert_eq!(outcome.deleted.len(), 1, "the confirmed source is deleted");
        assert!(!fx.src.exists(), "the file is removed from disk");
        assert_eq!(file_count(&store, fx.file_id), 0, "the files row is deleted");
        assert_eq!(frame_count(&store, fx.file_id), 0, "the frames row is CASCADE-deleted");
        assert_eq!(history_outcome_count(&store, "sent"), 1, "the transfer event survives");
        assert_eq!(history_outcome_count(&store, "retention_deleted"), 1, "the deletion event is recorded");
    }

    // ── deleter contract: audit-before-delete ────────────────────────────────

    /// If the audit row can't be persisted (here: the `sync_history` table is
    /// dropped), the delete is refused — the file and its catalog rows survive,
    /// nothing is counted. Proves audit-before-delete without a mock.
    #[test]
    fn audit_persist_failure_prevents_delete() {
        let fx = make_fixture(true);
        let store = CatalogSyncStore::open(&fx.db_path).unwrap();
        {
            let conn = store.lock_conn();
            conn.execute("DROP TABLE sync_history", []).unwrap();
        }

        let outcome = evaluate(&store, &cfg(RetentionPolicy::OnConfirm, false, true), Utc::now(), &|| 0u8)
            .unwrap();

        assert!(outcome.deleted.is_empty(), "an unpersistable audit must refuse the delete");
        assert!(fx.src.exists(), "the file survives when the audit can't be written");
        assert_eq!(file_count(&store, fx.file_id), 1, "the catalog row survives too");
    }

    // ── deleter contract: TOCTOU stat guard ──────────────────────────────────

    /// A source rewritten (new size/mtime) after confirmation is skipped — the
    /// stat guard compares against what was recorded at build time. The rewritten
    /// (unsynced) content survives, no catalog row is touched, no audit row.
    #[test]
    fn stat_guard_skips_a_source_changed_since_confirmation() {
        let fx = make_fixture(true);
        let store = CatalogSyncStore::open(&fx.db_path).unwrap();
        // Rewrite the source with different content (size changes → stat mismatch).
        std::fs::write(&fx.src, b"brand-new-unconfirmed-content").unwrap();

        let outcome = evaluate(&store, &cfg(RetentionPolicy::OnConfirm, false, true), Utc::now(), &|| 0u8)
            .unwrap();

        assert!(outcome.deleted.is_empty(), "a stat-mismatched source is never deleted");
        assert!(fx.src.exists(), "the rewritten content survives");
        assert_eq!(file_count(&store, fx.file_id), 1, "the catalog row is untouched");
        assert_eq!(history_outcome_count(&store, "retention_deleted"), 0, "no audit for a skipped delete");
    }

    // ── deleter contract: honest DeleteOutcome (already-handled no-op) ────────

    /// A package whose linkage was already stamped deleted is a legitimate no-op:
    /// it is a genuine confirmed candidate (still `eligible`) but is NOT counted
    /// as deleted and writes no duplicate audit row.
    #[test]
    fn already_handled_linkage_is_eligible_but_not_counted() {
        let fx = make_fixture(true);
        let store = CatalogSyncStore::open(&fx.db_path).unwrap();
        {
            let conn = store.lock_conn();
            mark_sync_source_deleted(&conn, &fx.package_ref, &fx.src.to_string_lossy()).unwrap();
        }

        let outcome = evaluate(&store, &cfg(RetentionPolicy::OnConfirm, false, true), Utc::now(), &|| 0u8)
            .unwrap();

        assert_eq!(outcome.eligible.len(), 1, "still a genuine confirmed candidate");
        assert!(outcome.deleted.is_empty(), "an already-handled package is never recounted as deleted");
        assert!(fx.src.exists(), "the file — logically already gone — is left untouched");
        assert_eq!(history_outcome_count(&store, "retention_deleted"), 0, "no duplicate audit row");
    }

    // ── A8 invariant suite over the app deleter ──────────────────────────────

    /// The hard invariant: an UNCONFIRMED source is never eligible and never
    /// deleted, even in live mode at maximum disk pressure.
    #[test]
    fn unconfirmed_source_never_eligible_even_on_full_disk() {
        let fx = make_fixture(false); // NOT confirmed
        let store = CatalogSyncStore::open(&fx.db_path).unwrap();

        let outcome = evaluate(
            &store,
            &cfg(RetentionPolicy::DiskPct { max_pct: 50 }, false, true),
            Utc::now(),
            &|| 99u8,
        )
        .unwrap();

        assert!(outcome.eligible.is_empty(), "unconfirmed is never eligible");
        assert!(outcome.deleted.is_empty());
        assert!(outcome.would_warn_disk_pressure, "full disk + nothing to free warns");
        assert!(fx.src.exists(), "the unconfirmed source survives a full disk in live mode");
        assert_eq!(file_count(&store, fx.file_id), 1, "its catalog row survives too");
    }

    /// `keep_everything` deletes nothing even with a confirmed source and a full
    /// disk in live mode.
    #[test]
    fn keep_everything_never_deletes() {
        let fx = make_fixture(true);
        let store = CatalogSyncStore::open(&fx.db_path).unwrap();

        let outcome = evaluate(&store, &cfg(RetentionPolicy::KeepEverything, false, true), Utc::now(), &|| 99u8)
            .unwrap();

        assert!(outcome.eligible.is_empty());
        assert!(outcome.deleted.is_empty());
        assert!(fx.src.exists(), "keep_everything never deletes");
        assert_eq!(file_count(&store, fx.file_id), 1);
    }

    /// Dry-run (the default) reports the confirmed candidate but deletes nothing
    /// and writes no audit row — even with the policy that would otherwise delete.
    #[test]
    fn dry_run_reports_but_deletes_nothing() {
        let fx = make_fixture(true);
        let store = CatalogSyncStore::open(&fx.db_path).unwrap();

        // dry_run=false but NO opt-in → effective dry-run (the safety net).
        let outcome = evaluate(&store, &cfg(RetentionPolicy::OnConfirm, false, false), Utc::now(), &|| 0u8)
            .unwrap();

        assert!(outcome.dry_run, "no opt-in → effective dry-run");
        assert_eq!(outcome.eligible.len(), 1, "reports the confirmed candidate");
        assert!(outcome.deleted.is_empty(), "dry-run deletes nothing");
        assert!(fx.src.exists(), "the file remains on disk");
        assert_eq!(file_count(&store, fx.file_id), 1, "the catalog row remains");
        assert_eq!(history_outcome_count(&store, "retention_deleted"), 0, "no audit in dry-run");
    }

    /// The deleter is invoked exactly for the confirmed candidate and reports an
    /// honest count — a smoke test that the store→deleter wiring counts `Removed`
    /// once per real removal (A8's "deleter counts calls" in live mode).
    #[test]
    fn deleter_removes_confirmed_source_once() {
        let fx = make_fixture(true);
        let store = CatalogSyncStore::open(&fx.db_path).unwrap();
        let calls = Cell::new(0usize);

        // Wrap evaluate's real deleter path but observe the outcome count.
        let outcome = evaluate(&store, &cfg(RetentionPolicy::OnConfirm, false, true), Utc::now(), &|| {
            calls.set(calls.get() + 1);
            0u8
        })
        .unwrap();
        // disk_probe isn't consulted by OnConfirm, so calls stays 0 — the honest
        // signal is the deleted count.
        assert_eq!(calls.get(), 0, "OnConfirm never probes disk");
        assert_eq!(outcome.deleted.len(), 1, "exactly the one confirmed source removed");
        assert!(!fx.src.exists());
    }
}
