//! Trigger policy for the whole-library content-hash index.
//!
//! `files.content_hash` has two consumers — the device-to-device transfer dedup
//! handshake, and content-based grouping in the Duplicates view when
//! `duplicates.use_content_hash` is on. Neither is worth paying for on every
//! scan, so the index is not part of scanning (which used to hash
//! unconditionally, at 3 x 512 KB of disk reads per file), and the AUTOMATIC
//! trigger does not fire at all on a node that has never configured sync — a
//! node that wants the column only for duplicate grouping starts the job by
//! hand from Settings, which is deliberately ungated. When it does run it is a
//! first-class visible job: it takes a `ComputeQueue` ticket, so it shows up in
//! the sidebar with a cancel button and can't fight a master build for the disk.

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

/// Catalogs whose pass the USER cancelled in this process. While a DB path is
/// in here, [`autostart_content_index`] does nothing for it.
///
/// A cancel is the one exit that must not feed [`LAST_UNHASHABLE`]: a pass
/// stopped after one chunk reports `skipped ~ 0`, and as a baseline that reads
/// as "this catalog has almost nothing unhashable", so the very next trigger —
/// boot, ANY scan, or a monitor cycle that ingests files (roughly every ten
/// minutes on a monitored capture library) — would see `pending > 0` and
/// restart the whole pass. The user pressed X on a job that reads ~1.5 MB per
/// catalogued file and it would come straight back, with no off switch short of
/// un-configuring sync. It would also destroy a GOOD baseline left by an earlier
/// complete pass, turning convergence off for the rest of the process.
///
/// Cancelling a `ContentIndex` job is always deliberate user intent: nothing in
/// core cancels one, and `ComputeQueue::cancel` reaches it only through
/// `cancel_compute_job` (the sidebar card's X).
///
/// Per process, deliberately NOT persisted — same reasoning as
/// [`LAST_UNHASHABLE`]: a cancel means "not now", not "never again", so a
/// relaunch earns one fresh attempt. Within the session the user is in charge:
/// the manual "Build index now" seam in each host clears the entry through
/// [`clear_cancelled_by_user`], so the button always works.
static CANCELLED_BY_USER: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

fn cancelled_set() -> &'static Mutex<HashSet<PathBuf>> {
    CANCELLED_BY_USER.get_or_init(|| Mutex::new(HashSet::new()))
}

fn lock_cancelled() -> MutexGuard<'static, HashSet<PathBuf>> {
    cancelled_set().lock().unwrap_or_else(|poisoned| {
        tracing::warn!("content index cancelled-set mutex was poisoned; recovering");
        poisoned.into_inner()
    })
}

/// Remember that the user cancelled this catalog's pass.
fn record_cancelled_by_user(path: &Path) {
    lock_cancelled().insert(path.to_path_buf());
}

/// Forget a cancel — the user changed their mind and asked for a pass by hand.
///
/// Called from the two MANUAL seams (the Tauri command and its Axum mirror),
/// never from inside [`start_content_index`], because that function serves both
/// the button AND [`autostart_content_index`]. Clearing in there would tie the
/// clear to whoever wins the single-flight claim rather than to user intent, and
/// that is a real window: a cancelling worker records the marker after an
/// in-flight autostart has already read it as absent, the autostart then claims
/// the slot, and — clearing there — it would erase the suppression the user just
/// earned and run the full pass anyway. Autostart never touches this function,
/// so pressing the button is the only thing that clears.
pub fn clear_cancelled_by_user(path: &Path) {
    lock_cancelled().remove(path);
}

/// Whether the user cancelled this catalog's pass earlier in this process.
fn was_cancelled_by_user(path: &Path) -> bool {
    lock_cancelled().contains(path)
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
/// no sync configured, AND after the user cancelled an earlier pass. The gate
/// and the cancel suppression both live in [`autostart_content_index`]; the
/// matching clear lives at the two manual seams (see
/// [`clear_cancelled_by_user`]) rather than here, because this function serves
/// the automatic trigger too.
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
                // A cancel before admission is still a cancel: suppress the
                // automatic re-arm, exactly as the running path below does, or
                // the next trigger would re-queue what the user just dropped.
                record_cancelled_by_user(database.path());
                tracing::info!("content index cancelled while queued; autostart suppressed");
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
        if summary.cancelled {
            // A partial pass's `skipped` is not a baseline (see
            // CANCELLED_BY_USER): record nothing, and leave whatever an earlier
            // complete pass established in place.
            record_cancelled_by_user(database.path());
            tracing::info!(
                updated = summary.updated,
                skipped = summary.skipped,
                "content index cancelled by user; autostart suppressed"
            );
        } else {
            // Written BEFORE the guard drops (it is bound first, so it drops
            // last). That ordering proves one thing and only one: an autostart
            // that observes the slot free also observes THIS pass's baseline.
            // It says nothing about the other operand — `autostart_content_index`
            // reads `pending` before it reads the baseline, so a pass completing
            // inside that window can still let one redundant re-arm through.
            // Bounded (that pass finds nothing to do, records the same baseline,
            // and the next trigger is quiet again) and it errs towards indexing
            // rather than towards leaving hashes unfilled.
            record_unhashable(&database, summary.skipped);
        }
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
    // Checked BEFORE `count_pending`, so a cancelled catalog costs nothing per
    // trigger — and this fires on every scan and every monitor cycle.
    if was_cancelled_by_user(database.path()) {
        tracing::debug!("content index autostart skipped: the user cancelled a pass this session");
        return;
    }
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

    /// Emitter that cancels the running content-index job the first time the
    /// pass reports progress — from inside the pass's OWN thread, at a point
    /// where work provably remains (chunk 1 of 2 has just been written).
    ///
    /// Deterministic where a cancel timed from the test thread would be a race:
    /// the flag is set before the pass can reach its next cancel check, so the
    /// pass always ends `cancelled` with rows still NULL. It goes through the
    /// real `ComputeQueue::cancel`, the same call the sidebar's X makes.
    struct CancelOnFirstProgress {
        queue: ComputeQueue,
        fired: AtomicBool,
    }

    impl ProgressEmitter for CancelOnFirstProgress {
        fn emit_json(&self, event_name: &str, _payload: serde_json::Value) {
            use std::sync::atomic::Ordering;
            if event_name != "content-index-progress" || self.fired.swap(true, Ordering::SeqCst) {
                return;
            }
            let job = self
                .queue
                .snapshot()
                .into_iter()
                .find(|e| e.kind == ComputeJobKind::ContentIndex)
                .expect("the running pass must be visible in the compute queue");
            assert!(self.queue.cancel(job.job_id), "cancel must find the job");
        }
    }

    /// A user's cancel STAYS cancelled. The pass is the loudest disk consumer
    /// in the app (~1.5 MB read per catalogued file), and its automatic
    /// triggers — boot, every scan, every monitor cycle that ingests files —
    /// fire often enough that a re-arm within the minute is the normal case, so
    /// pressing X has to switch it off, not pause it. Recording the partial
    /// pass's `skipped` as the convergence baseline is what used to undo the
    /// cancel: ~0 unhashable against a large `pending` reads as "lots of new
    /// work".
    ///
    /// The second half is the other half of the contract: the manual "Build
    /// index now" button is the user changing their mind and must always work.
    #[test]
    fn a_user_cancel_suppresses_autostart_until_a_manual_start() {
        // 70 rows = two chunks (CHUNK = 64), so the first progress event lands
        // with rows still unhashed and the cancel has something to interrupt.
        let (ctx, _tmp) = test_ctx_with_files(70);
        sign_in(&ctx);
        let database = ctx.db.get().unwrap().clone();

        assert!(start_content_index(
            database.clone(),
            ctx.compute_queue.clone(),
            Arc::new(CancelOnFirstProgress {
                queue: ctx.compute_queue.clone(),
                fired: AtomicBool::new(false),
            }),
        ));
        wait_until("the cancelled pass releases the single-flight slot", || {
            !get_content_index_status(&ctx).unwrap().running
        });
        let left_behind = get_content_index_status(&ctx).unwrap().pending;
        assert!(
            left_behind > 0,
            "the cancel must have stopped the pass with work left (pending = {left_behind})"
        );

        // The trigger that used to undo the cancel. Deterministic: a start
        // claims the single-flight slot synchronously, before it spawns.
        autostart_content_index(&ctx, Arc::new(NullEmitter));
        assert!(
            !get_content_index_status(&ctx).unwrap().running,
            "a cancelled pass must not be re-armed automatically"
        );
        // Belt and braces: a bounded window in which a resurrected pass (6 tiny
        // files) would have finished and shown up in `pending`. This asserts
        // that something did NOT happen, so there is no state to poll for.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            get_content_index_status(&ctx).unwrap().pending,
            left_behind,
            "nothing may have been hashed after the cancel"
        );

        // ...and the manual button overrides it, for this pass AND for the
        // automatic triggers that follow. Driven through `manual_start`, the
        // clear-then-start pair both hosts spell out at their own boundary.
        assert!(manual_start(
            &database,
            ctx.compute_queue.clone(),
            Arc::new(NullEmitter)
        ));
        wait_until("the manual pass indexes what the cancel left", || {
            get_content_index_status(&ctx).unwrap().pending == 0
        });
        wait_until("the manual pass releases the single-flight slot", || {
            !get_content_index_status(&ctx).unwrap().running
        });
        assert!(
            !was_cancelled_by_user(database.path()),
            "a manual start must clear the suppression, not merely bypass it"
        );

        // The suppression really is gone, not just stepped over: the automatic
        // trigger works again. (Nothing is pending now, so what this asserts is
        // that the gate ran to the end — a still-suppressed catalog returns at
        // the marker check, above `count_pending`.)
        autostart_content_index(&ctx, Arc::new(NullEmitter));
        assert_eq!(
            get_content_index_status(&ctx).unwrap().pending,
            0,
            "the catalog stays fully indexed"
        );
    }

    /// What both host wrappers do for a manual "Index now": clear the cancel
    /// marker, then start. The clear lives at the host seam BECAUSE
    /// `start_content_index` also serves the autostart (see
    /// [`clear_cancelled_by_user`]), so a test that called only
    /// `start_content_index` would pin the wrong contract.
    ///
    /// Mirrors `athenaeum-tauri/src/commands/content_index.rs` and
    /// `athenaeum-web/src/routes/content_index.rs`, kept in step by hand like
    /// every other two-backend seam here.
    fn manual_start(
        database: &Database,
        queue: ComputeQueue,
        emitter: Arc<dyn ProgressEmitter>,
    ) -> bool {
        clear_cancelled_by_user(database.path());
        start_content_index(database.clone(), queue, emitter)
    }
}
