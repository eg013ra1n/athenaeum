//! Trigger policy for the whole-library content-hash index.
//!
//! `files.content_hash` has exactly one consumer — the device-to-device
//! transfer dedup handshake. So the index is not part of scanning (which used
//! to hash unconditionally, at 3 x 512 KB of disk reads per file) and it does
//! not run at all on a node that has never configured sync. When it does run it
//! is a first-class visible job: it takes a `ComputeQueue` ticket, so it shows
//! up in the sidebar with a cancel button and can't fight a master build for
//! the disk.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::duplicates::backfill::ContentIndexFinished;
use crate::events::{emit_event, ProgressEmitter};
use crate::services::compute_queue::{ComputeJobKind, ComputeQueue};
use crate::services::ServiceContext;

use super::{db, ApiError};

/// DB paths with a pass in flight.
static RUNNING_FOR: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

fn running_set() -> &'static Mutex<HashSet<PathBuf>> {
    RUNNING_FOR.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Lock the running-set, recovering from a poisoned mutex rather than
/// panicking. The set holds nothing but database paths — a panic elsewhere
/// cannot leave it logically inconsistent — and this lock is also taken from
/// [`RunningGuard::drop`], where an `expect` would abort the process outright
/// if it ran during an unwind. Recover, and say so, instead of turning a leaked
/// entry into a crash.
fn lock_running() -> MutexGuard<'static, HashSet<PathBuf>> {
    running_set().lock().unwrap_or_else(|poisoned| {
        tracing::warn!("content index running-set mutex was poisoned; recovering");
        poisoned.into_inner()
    })
}

/// RAII claim on the single-flight slot of one catalog.
///
/// Keyed by database PATH, not by a process-global bool, so a dev DB-path swap
/// can still index its own catalog (same reasoning as the guard this replaces
/// in [`crate::duplicates::backfill`]).
///
/// `Drop` is what releases the claim, deliberately: the worker thread ends in
/// several ways — cancelled while queued, finished, or panicked inside the pass
/// — and a manual `remove` at the bottom of the body would leak the entry on
/// the panic path, wedging the job for that catalog until the app restarts.
struct RunningGuard(PathBuf);

impl RunningGuard {
    /// `Some` if this call took the slot, `None` if a pass already holds it.
    fn claim(path: &Path) -> Option<Self> {
        if lock_running().insert(path.to_path_buf()) {
            Some(RunningGuard(path.to_path_buf()))
        } else {
            None
        }
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        lock_running().remove(&self.0);
    }
}

/// Rows the last completed pass could NOT hash, per DB path — the re-arm
/// baseline (see [`autostart_content_index`]). Same poison-recovering
/// discipline as the running set, and for the same reason: it is written from
/// the worker thread, whose panic must not turn into a dead app.
static LAST_UNHASHABLE: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();

fn unhashable_map() -> &'static Mutex<HashMap<PathBuf, usize>> {
    LAST_UNHASHABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_unhashable() -> MutexGuard<'static, HashMap<PathBuf, usize>> {
    unhashable_map().lock().unwrap_or_else(|poisoned| {
        tracing::warn!("content index unhashable-map mutex was poisoned; recovering");
        poisoned.into_inner()
    })
}

/// How many rows the last completed pass on this catalog left unhashed.
/// `None` until one has completed in this process.
fn last_unhashable(database: &Database) -> Option<usize> {
    lock_unhashable().get(database.path()).copied()
}

fn record_unhashable(database: &Database, skipped: usize) {
    lock_unhashable().insert(database.path().to_path_buf(), skipped);
}

/// Whether a pass currently holds the slot for `path`.
fn is_running(path: &Path) -> bool {
    lock_running().contains(path)
}

/// What the Settings card renders.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ContentIndexStatus {
    /// Catalogued files still missing a hash.
    pub pending: i64,
    /// Catalogued files in total.
    pub total: i64,
    /// A pass holds this catalog's single-flight slot: either waiting for a
    /// compute-queue ticket or hashing. Queued counts as running on purpose —
    /// this is what refuses a second start.
    pub running: bool,
    /// Whether the job runs automatically on this node.
    pub sync_configured: bool,
}

// No `#[tracing::instrument]` here — boundary spans live on the Tauri command
// and the Axum route (see api/mod.rs:2). Adding one here would produce a
// duplicate nested span and double error events.
pub fn get_content_index_status(ctx: &ServiceContext) -> Result<ContentIndexStatus, ApiError> {
    let database = db(ctx)?;
    let conn = database.conn();
    // This read doubles as the honest-failure probe for the pending count
    // below: `count_pending` logs and returns 0 when its own query fails, which
    // the card would render as "index complete" over a broken catalog. Hitting
    // the same table here with `?` turns a locked / missing / unreadable
    // `files` into a failed status call instead of a comfortable lie. (One
    // residual case survives: a `files` table readable but missing the
    // `content_hash` column — a shape `init_db` does not produce.)
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
    drop(conn);
    let pending = crate::duplicates::backfill::count_pending(database) as i64;
    Ok(ContentIndexStatus {
        pending,
        total,
        running: is_running(database.path()),
        sync_configured: crate::api::sync::sync_configured(ctx)?,
    })
}

/// Start a pass on a background thread. Returns `false` when one is already in
/// flight for this database — the boot autostart and the post-scan re-arm can
/// both fire in the same second, and doubling the disk load would be exactly
/// the behaviour this whole change exists to remove.
///
/// NOT gated: a manual "Index now" from Settings must work on a node that has
/// no sync configured. The gate lives in [`autostart_content_index`].
pub fn start_content_index(
    database: Database,
    queue: ComputeQueue,
    emitter: Arc<dyn ProgressEmitter>,
) -> bool {
    let Some(guard) = RunningGuard::claim(database.path()) else {
        tracing::debug!(
            path = %database.path().display(),
            "content index already running; ignoring start"
        );
        return false;
    };

    std::thread::spawn(move || {
        // Released on every exit of this body, panics included — see RunningGuard.
        let _guard = guard;
        let cancel = Arc::new(AtomicBool::new(false));
        // Admission first: an IO-heavy whole-library pass must not run beside a
        // master build. A ticket cancelled while queued never runs the pass.
        let permit = match queue.acquire(
            ComputeJobKind::ContentIndex,
            "Content index",
            cancel.clone(),
        ) {
            Ok((permit, job_id)) => {
                tracing::info!(job_id, "content index admitted");
                permit
            }
            // `QueueCancelled` is a user decision, not a failure, and carries no
            // detail to log beyond the fact itself.
            Err(_) => {
                tracing::info!("content index cancelled while queued");
                // Task 2's invariant: `content-index-finished` fires on EVERY
                // exit path, so consumers have exactly one place to close on.
                // This branch is an exit the pass itself never sees — and
                // `start_content_index` has already returned `true` — so
                // without this a card that arms on start would stay armed
                // forever.
                emit_event(
                    emitter.as_ref(),
                    "content-index-finished",
                    &ContentIndexFinished {
                        updated: 0,
                        skipped: 0,
                        cancelled: true,
                        failed: false,
                    },
                );
                return;
            }
        };

        let summary =
            crate::duplicates::backfill::run_content_index(&database, emitter.as_ref(), cancel);
        // Written BEFORE the guard drops (it is bound first, so it drops last),
        // which is what makes the re-arm check race-free: any autostart that
        // observes the slot free also observes this pass's baseline.
        record_unhashable(&database, summary.skipped);
        drop(permit);
    });

    true
}

/// The gated entry point. Both hosts call this at boot and after every scan.
/// A node with no sync configured pays nothing.
pub fn autostart_content_index(ctx: &ServiceContext, emitter: Arc<dyn ProgressEmitter>) {
    let configured = match crate::api::sync::sync_configured(ctx) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "content index autostart: gate check failed");
            return;
        }
    };
    if !configured {
        tracing::debug!("content index autostart skipped: sync not configured");
        return;
    }
    let Some(database) = ctx.db.get().cloned() else {
        tracing::debug!("content index autostart skipped: database not initialised");
        return;
    };
    // Re-arm on WORK A PASS CAN ACTUALLY DO, not on NULL-hash rows.
    //
    // `count_pending` counts every `files` row with a NULL hash, but the pass
    // permanently skips rows whose file is missing on disk or whose
    // (size, modified_at) drifted — it leaves those NULL by design, because
    // "missing != orphan" is a project rule and a disconnected drive must never
    // cost catalog rows. Gating on `pending == 0` therefore never converges on
    // any catalog with an offline drive: the job would re-fire at every boot AND
    // after every scan, for the life of the install.
    //
    // So compare against what the previous pass could not hash, and let the
    // pass's own skip logic be the single source of truth for "unhashable".
    // The record is PER PROCESS, deliberately not persisted: a reconnected drive
    // gets one fresh attempt at the next launch, while within a session the
    // degenerate catalog settles after exactly one pass.
    let pending = crate::duplicates::backfill::count_pending(&database);
    if pending == 0 {
        tracing::debug!("content index autostart skipped: nothing pending");
        return;
    }
    if let Some(unhashable) = last_unhashable(&database) {
        if pending <= unhashable {
            tracing::debug!(
                pending,
                unhashable,
                "content index autostart skipped: no newly hashable rows"
            );
            return;
        }
    }
    if start_content_index(database, ctx.compute_queue.clone(), emitter) {
        tracing::info!(pending, "content index autostart");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::events::NullEmitter;

    /// A `ServiceContext` over a temp DB with `n` real files catalogued and
    /// unhashed.
    ///
    /// Real bytes on disk, because the pass's stale-row guard compares the
    /// row's `(size, modified_at)` against the file's — a fabricated row would
    /// simply be skipped, and the "nothing was hashed" assertions below would
    /// then pass for the wrong reason.
    ///
    /// Built with `ServiceContext::new_for_tests` rather than a struct literal:
    /// the solver/render cache fields are `cfg`-gated, and this module is
    /// ungated, so a literal would stop compiling in a headless
    /// (`--no-default-features`) build of the crate.
    fn test_ctx_with_files(n: usize) -> (ServiceContext, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("t.db"));
        {
            let database = ctx.db.get().unwrap();
            let conn = database.conn();
            for i in 0..n {
                let p = tmp.path().join(format!("f{i}.fits"));
                crate::archive::restore::tests::write_minimal_fits(&p);
                insert_row(&conn, &p);
            }
        }
        (ctx, tmp)
    }

    /// Catalog `path` as a `files` row with a NULL hash, stamped with the
    /// file's REAL `(size, modified_at)` when it exists. A path with nothing on
    /// disk is the disconnected-drive shape: the pass skips it permanently and
    /// leaves the row NULL, because "missing != orphan".
    fn insert_row(conn: &rusqlite::Connection, path: &Path) {
        let (size, modified) = match std::fs::metadata(path) {
            Ok(m) => (
                m.len() as i64,
                chrono::DateTime::<chrono::Utc>::from(m.modified().unwrap()).to_rfc3339(),
            ),
            Err(_) => (1024, "2026-08-11T00:00:00Z".to_string()),
        };
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, created_at)
             VALUES (?1, ?2, ?3, ?4, 'FITS', ?5)",
            rusqlite::params![
                path.to_str().unwrap(),
                path.file_name().unwrap().to_str().unwrap(),
                size,
                modified,
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .unwrap();
    }

    /// Open the autostart gate exactly the way the app does — the persisted
    /// `ACCOUNT_DEVICE_ID` sign-in marker.
    fn sign_in(ctx: &ServiceContext) {
        let database = ctx.db.get().unwrap();
        let conn = database.conn();
        crate::db::set_setting(&conn, crate::settings::keys::ACCOUNT_DEVICE_ID, "device-1")
            .unwrap();
    }

    /// Poll a real state change, bounded and loud on timeout. Used only where
    /// there is genuine state to observe — never as a blind sleep standing in
    /// for a wait.
    fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !cond() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting until {what}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Status is honest about the gate: a node with no sync configured reports
    /// `syncConfigured: false`, which is what the Settings card renders its
    /// "not running automatically" explanation from.
    #[test]
    fn status_reports_pending_and_gate() {
        let (ctx, _tmp) = test_ctx_with_files(2);
        let status = get_content_index_status(&ctx).unwrap();
        assert_eq!(status.total, 2);
        assert_eq!(status.pending, 2);
        assert!(!status.running);
        assert!(
            !status.sync_configured,
            "no ACCOUNT_DEVICE_ID => gate closed"
        );
    }

    /// Signing in opens the gate — same predicate as the receiver autostart.
    #[test]
    fn status_gate_opens_when_signed_in() {
        let (ctx, _tmp) = test_ctx_with_files(1);
        sign_in(&ctx);
        assert!(get_content_index_status(&ctx).unwrap().sync_configured);
    }

    /// Single-flight: a second start while one is running is a no-op, so a boot
    /// autostart racing a post-scan re-arm can't double the disk load. The
    /// claim is taken through the real RAII guard, not a test-only backdoor.
    #[test]
    fn start_is_single_flight_per_database() {
        let (ctx, _tmp) = test_ctx_with_files(0);
        let database = ctx.db.get().unwrap().clone();
        let claim = RunningGuard::claim(database.path()).expect("first claim wins");
        assert!(
            !start_content_index(
                database.clone(),
                ctx.compute_queue.clone(),
                Arc::new(NullEmitter)
            ),
            "a second start while running must be refused"
        );
        assert!(
            get_content_index_status(&ctx).unwrap().running,
            "status must report the in-flight pass"
        );
        drop(claim);
        assert!(
            start_content_index(database, ctx.compute_queue.clone(), Arc::new(NullEmitter)),
            "once the guard clears, a start is accepted again"
        );

        // That start owns a real worker thread. Wait for it to release the
        // slot: it covers the ordinary non-panic release path, and it keeps the
        // worker from outliving the temp catalog it is reading.
        wait_until("the worker releases the single-flight slot", || {
            !get_content_index_status(&ctx).unwrap().running
        });
    }

    /// The single-flight claim is released even if the pass unwinds. A manual
    /// `remove` at the end of the worker would leak the entry on panic and the
    /// job could never start again for that catalog until the app restarts.
    ///
    /// The panic message is written by the default hook onto this test thread's
    /// captured stderr, which libtest prints only for a FAILING test — a
    /// passing run stays quiet, so no global panic-hook surgery is needed.
    #[test]
    fn running_guard_is_released_on_panic() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("panicking.db");
        let outcome = std::panic::catch_unwind(|| {
            let _claim = RunningGuard::claim(&path).expect("first claim wins");
            panic!("content index pass exploded");
        });
        assert!(outcome.is_err(), "the closure must actually have panicked");
        assert!(
            RunningGuard::claim(&path).is_some(),
            "an unwound pass must leave the slot claimable"
        );
    }

    /// The gate is enforced at the autostart entry point, not inside the job:
    /// a manual start from Settings still works on an ungated node.
    #[test]
    fn autostart_is_a_noop_when_sync_is_not_configured() {
        let (ctx, _tmp) = test_ctx_with_files(2);
        autostart_content_index(&ctx, Arc::new(NullEmitter));

        // Deterministic, not timing-dependent: `start_content_index` claims the
        // single-flight slot SYNCHRONOUSLY, before it spawns, so a pass that
        // had slipped past the gate would already be visible here.
        assert!(
            !get_content_index_status(&ctx).unwrap().running,
            "gate closed: no pass may have been started"
        );
        // Belt and braces. This asserts that something did NOT happen, so there
        // is no state to poll for: the wait is a bounded window in which a pass
        // (2 tiny files, milliseconds of work) would have finished and shown up
        // in `pending`, not a poll-until-true masking a race.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            get_content_index_status(&ctx).unwrap().pending,
            2,
            "gate closed: nothing should have been hashed"
        );
    }

    /// The headline happy path: a node WITH sync configured and pending work
    /// actually indexes. Every other autostart test here is a negative, and a
    /// gate that never opened at all would satisfy all of them.
    #[test]
    fn autostart_runs_a_pass_when_sync_is_configured() {
        let (ctx, _tmp) = test_ctx_with_files(2);
        sign_in(&ctx);
        assert_eq!(get_content_index_status(&ctx).unwrap().pending, 2);

        autostart_content_index(&ctx, Arc::new(NullEmitter));

        // Assert the OUTCOME, not "running == true" right after the call: the
        // worker may legitimately finish before the test thread looks again.
        // A closed gate makes this wait time out loudly instead.
        wait_until("every hashable row carries a hash", || {
            get_content_index_status(&ctx).unwrap().pending == 0
        });
        wait_until("the pass releases the single-flight slot", || {
            !get_content_index_status(&ctx).unwrap().running
        });
    }

    /// Convergence. A catalog whose only pending rows can never be hashed — the
    /// disconnected-drive shape, which this project explicitly supports and
    /// never auto-purges — must run ONE pass and then stop re-arming. Gating on
    /// `pending == 0` instead would re-fire this job at every boot and after
    /// every scan, for the life of the install.
    #[test]
    fn autostart_converges_when_every_pending_row_is_unhashable() {
        let (ctx, tmp) = test_ctx_with_files(0);
        {
            let database = ctx.db.get().unwrap();
            let conn = database.conn();
            for i in 0..2 {
                insert_row(&conn, &tmp.path().join(format!("offline{i}.fits")));
            }
        }
        sign_in(&ctx);

        autostart_content_index(&ctx, Arc::new(NullEmitter));
        wait_until("the first pass releases the single-flight slot", || {
            !get_content_index_status(&ctx).unwrap().running
        });
        assert_eq!(
            get_content_index_status(&ctx).unwrap().pending,
            2,
            "unhashable rows must stay NULL — missing != orphan"
        );

        // Second call: still 2 pending, none of them newly hashable, so no pass.
        // Deterministic — a start would have claimed the slot synchronously.
        autostart_content_index(&ctx, Arc::new(NullEmitter));
        assert!(
            !get_content_index_status(&ctx).unwrap().running,
            "a catalog with only unhashable rows must not re-arm"
        );

        // ...and it re-arms the moment there IS new work: one real file pushes
        // `pending` above the unhashable baseline. Without this phase an
        // unconditional "never re-arm again" would satisfy the assertion above.
        {
            let database = ctx.db.get().unwrap();
            let conn = database.conn();
            let fresh = tmp.path().join("fresh.fits");
            crate::archive::restore::tests::write_minimal_fits(&fresh);
            insert_row(&conn, &fresh);
        }
        autostart_content_index(&ctx, Arc::new(NullEmitter));
        wait_until("the re-armed pass hashes the newly scanned file", || {
            get_content_index_status(&ctx).unwrap().pending == 2
        });
    }
}
