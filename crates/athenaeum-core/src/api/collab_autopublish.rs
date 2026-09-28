//! Coalesced auto-publish per project (collab v3 wave 2, Task 10; R16, §5.2
//! triggers; P13 — `collab_projects.auto_publish` is a LOCAL preference,
//! default ON, never overwritten by a hub refresh).
//!
//! One debounced, re-armed publish run per process: scan completion,
//! analysis completion, a plate-solve batch, a frame-set link, a master
//! build/rebuild, a calibration-link change and a project's thresholds or
//! dictionary moving each mark a project (or a frame set, or "every
//! project") dirty and kick the worker. The worker waits for a kick, debounces
//! [`AUTO_PUBLISH_DEBOUNCE`] (so a scan and its immediately-following
//! analysis coalesce into one run), drains the dirty state into the
//! `auto_publish = 1` projects that have at least one linked set, and runs
//! [`crate::api::collab::auto_publish_collab_frames`] for each, SEQUENTIALLY
//! (the compute queue already serializes the heavy part). A project with a
//! publish run in progress right then (a manual one) is NOT run in parallel:
//! it is marked dirty again and kicked, so it runs after that run ends
//! (final review C1, owner decision 2026-09-24). A request that arrives
//! DURING a run marks its project dirty again and is picked up by the next
//! run — the loop re-arms itself via [`tokio::sync::Notify`]'s single stored
//! permit (a `notify_one` while nobody waits is kept for the next wait).
//!
//! Gated the same as `api::collab` (render+solver, `api/mod.rs`): it drives
//! the render-gated calibrated-light generator via `publish_collab_frames`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::Notify;

use crate::account::client::COLLAB_API_OUTDATED_MSG;
use crate::api::collab::{auto_publish_collab_frames, PublishResult, PUBLISH_BUSY_MSG};
use crate::api::{db, ApiError};
use crate::events::ProgressEmitter;
use crate::services::ServiceContext;

/// Debounce between a kick and the drained run (worker step 2): long enough
/// that a scan's completion and its immediately-following analysis coalesce
/// into one publish run.
#[cfg_attr(test, allow(dead_code))]
const AUTO_PUBLISH_DEBOUNCE: Duration = Duration::from_secs(30);

// ── Dirty state (module statics: the producers, reached from every trigger
// site across the crate, and the worker have no shared owner) ─────────────

/// Project ids marked dirty directly (a project-scoped trigger: a link, a
/// thresholds/dictionary move, a manual "sync now"-style act).
static DIRTY: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Frame-set ids marked dirty by a set-scoped trigger (analysis, a
/// calibration-link change). Stored raw — the call sites that mark these
/// (e.g. `api::analysis`, `api::calibration`, both NOT always compiled with
/// a `ServiceContext` in scope at the exact call site) have no cheap way to
/// resolve set → project themselves; the worker resolves them at drain time,
/// where it already holds the one `ServiceContext` it needs.
static DIRTY_SETS: OnceLock<Mutex<HashSet<i64>>> = OnceLock::new();

/// Set by [`request_auto_publish(None)`] — a trigger too broad to cheaply map
/// to specific sets (a scan, a plate-solve batch, a whole-camera calibration
/// refresh). Drains to every project with `auto_publish = 1` and a linked
/// set, same filter as everything else.
static ALL_DIRTY: AtomicBool = AtomicBool::new(false);

/// The wakeup the auto-publish worker waits on.
static KICK: OnceLock<Notify> = OnceLock::new();

fn dirty() -> &'static Mutex<HashSet<String>> {
    DIRTY.get_or_init(|| Mutex::new(HashSet::new()))
}

fn dirty_sets() -> &'static Mutex<HashSet<i64>> {
    DIRTY_SETS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn kick() -> &'static Notify {
    KICK.get_or_init(Notify::new)
}

/// Mark one project dirty (`Some(project_id)`), or every auto-publish-enabled
/// project with a linked set (`None` — a trigger too broad or too expensive
/// to map to specific projects here). Non-blocking: this only records intent
/// and wakes the worker; the actual publish runs on the spawned worker task.
pub fn request_auto_publish(project_id: Option<&str>) {
    match project_id {
        Some(id) => {
            if let Ok(mut set) = dirty().lock() {
                set.insert(id.to_string());
            }
        }
        None => ALL_DIRTY.store(true, Ordering::SeqCst),
    }
    kick().notify_one();
}

/// Test-only peek at [`DIRTY`] that does NOT drain it (unlike
/// [`drain_due_projects`]) — a project-mapping save's assertion that
/// `request_auto_publish` actually ran, without racing the real drain.
#[cfg(test)]
pub(crate) fn is_dirty_for_test(project_id: &str) -> bool {
    dirty().lock().unwrap().contains(project_id)
}

/// [`is_dirty_for_test`]'s sibling for [`DIRTY_SETS`] — a set-scoped
/// trigger's (e.g. [`request_auto_publish_for_sets`]) assertion that a given
/// set id was actually marked dirty, without draining it.
#[cfg(test)]
pub(crate) fn is_set_dirty_for_test(set_id: i64) -> bool {
    dirty_sets().lock().unwrap().contains(&set_id)
}

/// Mark every project linked to any of the given frame sets dirty. The
/// mapping happens at drain time (the worker holds the `ServiceContext` this
/// needs); this call site only records the raw set ids.
pub fn request_auto_publish_for_sets(set_ids: &[i64]) {
    if set_ids.is_empty() {
        return;
    }
    if let Ok(mut set) = dirty_sets().lock() {
        set.extend(set_ids.iter().copied());
    }
    kick().notify_one();
}

/// Trigger site: a scan's completion (`api::scan_roots::start_scan_with_progress`).
/// A scan can create sessions, re-cluster frame sets and change a camera —
/// mapping that cheaply to "which linked projects care" from the scan result
/// alone isn't worth it, so this dirties every auto-publish-enabled project.
/// Cheap even when nothing changed for a given project: a publish run with
/// nothing new to generate takes no compute-queue permit (Task 7).
pub fn request_auto_publish_for_scan(_root_id: i64) {
    request_auto_publish(None);
}

/// Drain the dirty state into a de-duplicated, gate-filtered project id list
/// (worker step 3): resolves [`DIRTY_SETS`] to their linked projects, adds
/// every cached project when [`ALL_DIRTY`] was set, unions with [`DIRTY`],
/// then keeps only `auto_publish = 1` projects with at least one linked set.
/// A DB failure logs a `warn!` and yields an empty list for that half of the
/// drain rather than panicking the worker.
fn drain_due_projects(ctx: &ServiceContext) -> Vec<String> {
    let all = ALL_DIRTY.swap(false, Ordering::SeqCst);
    let mut ids: HashSet<String> = dirty()
        .lock()
        .map(|mut s| std::mem::take(&mut *s))
        .unwrap_or_default();
    let sets: HashSet<i64> = dirty_sets()
        .lock()
        .map(|mut s| std::mem::take(&mut *s))
        .unwrap_or_default();

    let Ok(database) = db(ctx) else {
        tracing::warn!("auto-publish: database unavailable; drain skipped");
        return Vec::new();
    };
    let conn = database.conn();

    if !sets.is_empty() {
        let set_ids: Vec<i64> = sets.into_iter().collect();
        match crate::db::collab::project_ids_for_sets(&conn, &set_ids) {
            Ok(mapped) => ids.extend(mapped),
            Err(error) => {
                tracing::warn!(error = %error, "auto-publish: set→project lookup failed");
            }
        }
    }
    if all {
        match crate::db::collab::list_projects(&conn) {
            Ok(projects) => ids.extend(projects.into_iter().map(|p| p.project_id)),
            Err(error) => {
                tracing::warn!(error = %error, "auto-publish: project list failed");
            }
        }
    }

    let mut due: Vec<String> = Vec::new();
    for project_id in ids {
        let project = match crate::db::collab::get_project(&conn, &project_id) {
            Ok(Some(p)) => p,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(project_id = %project_id, error = %error, "auto-publish: project lookup failed");
                continue;
            }
        };
        if !project.auto_publish {
            continue;
        }
        // R14: `get_project` still reads a lost project (only `list_projects`
        // filters it), and `project_links` survives a loss (only the replica
        // rows and collab-store tags are dropped) — so both DIRTY (a direct
        // mark, e.g. a stale `link_frame_set` before the next poll notices
        // the loss) and DIRTY_SETS (`project_ids_for_sets` reads
        // `project_links` raw, no `lost_at` join) can carry a lost project's
        // id here even though ALL_DIRTY's `list_projects` never would. Same
        // check `collab_exchange::live_project` uses.
        match crate::db::collab::lost_at(&conn, &project_id) {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(project_id = %project_id, error = %error, "auto-publish: lost-project lookup failed");
                continue;
            }
        }
        match crate::db::collab::linked_set_ids(&conn, &project_id) {
            Ok(links) if !links.is_empty() => due.push(project_id),
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(project_id = %project_id, error = %error, "auto-publish: linked-set lookup failed");
            }
        }
    }
    due.sort();
    due
}

/// One coalesced publish run over `due` projects (worker steps 4-7). Calls
/// `publish` once per project, sequentially — production binds a closure
/// around [`auto_publish_collab_frames`] (`spawn_auto_publish_worker`); tests
/// inject a recorder.
///
/// `publish` takes an owned `project_id: String` and closes over `ctx`
/// itself rather than receiving `&ServiceContext` as a parameter, because a
/// generic `Fn` bound over TWO independent borrowed parameters
/// (`&ServiceContext` and `&str`) does not unify against a plain async fn
/// item — the async fn's opaque return type is tied to one concrete
/// lifetime, not the higher-ranked `for<'a, 'b>` bound the generic needs.
///
/// Signed out, no Collaboration root, or a hub too old for this client
/// (`CollabApiOutdated`) are run-wide conditions — every one of `due`'s
/// projects would fail identically — so each logs exactly ONE `warn!` and
/// skips the rest of the run (worker step 7) instead of one `warn!` per
/// project. Any other per-project failure is logged and the loop continues
/// (worker step 6: never stops the worker).
async fn run_publish_pass<F, Fut>(
    ctx: &ServiceContext,
    emitter: Option<Arc<dyn ProgressEmitter>>,
    due: Vec<String>,
    publish: F,
) where
    F: Fn(String, Option<Arc<dyn ProgressEmitter>>) -> Fut,
    Fut: std::future::Future<Output = Result<PublishResult, ApiError>>,
{
    if due.is_empty() {
        return;
    }

    // Every early-return below (run-wide skip, or an outdated hub mid-loop)
    // re-marks whatever it didn't process dirty WITHOUT kicking (see
    // `remark_dirty_no_kick`): the condition is local/account-level, not
    // per-project, so busy-looping the debounce again right now would just
    // hit it again — the project is retried on the next real trigger, once
    // the condition has had a chance to clear.
    match crate::api::account::hub_credentials(ctx) {
        Ok(Some(_)) => {}
        Ok(None) => {
            tracing::warn!(count = due.len(), "auto-publish: signed out; run skipped");
            due.iter().for_each(|id| remark_dirty_no_kick(id));
            return;
        }
        Err(error) => {
            tracing::warn!(count = due.len(), error = %error, "auto-publish: account read failed; run skipped");
            due.iter().for_each(|id| remark_dirty_no_kick(id));
            return;
        }
    }
    if crate::api::collab_exchange::require_collaboration_root(ctx).is_err() {
        tracing::warn!(
            count = due.len(),
            "auto-publish: no Collaboration root; run skipped"
        );
        due.iter().for_each(|id| remark_dirty_no_kick(id));
        return;
    }

    let mut remaining = due.into_iter();
    while let Some(project_id) = remaining.next() {
        let result = publish(project_id.clone(), emitter.clone()).await;
        match result {
            Ok(result) => {
                tracing::info!(
                    project_id = %project_id,
                    announced = result.announced,
                    updated = result.updated,
                    unchanged = result.unchanged,
                    held_back = result.held_back.len(),
                    "auto-publish run complete"
                );
            }
            Err(ApiError::Conflict(msg)) if msg == PUBLISH_BUSY_MSG => {
                // C1: another publish run of this project holds its lock —
                // never run beside it. Re-mark and kick: the next drained run
                // (after the debounce) picks it up on the state it leaves.
                tracing::info!(
                    project_id = %project_id,
                    "auto-publish: a publish of this project is running; re-queued"
                );
                request_auto_publish(Some(&project_id));
            }
            Err(ApiError::Conflict(msg)) if is_publishing_device_refusal(&msg) => {
                // A6: another device of this account publishes into this
                // project. Quiet by design: the refusal is recorded with the
                // project (the next run stops before any work), and a moved
                // binding re-arms this project (`refresh_projects_reporting`,
                // `set_collab_publishing_device`) — never a retry loop.
                tracing::debug!(
                    project_id = %project_id,
                    outcome = "publishing_device",
                    "auto-publish: another device of this account publishes into this project; waiting for the binding to move"
                );
            }
            Err(ApiError::Conflict(msg)) if msg == COLLAB_API_OUTDATED_MSG => {
                tracing::warn!(
                    project_id = %project_id,
                    "auto-publish: hub API outdated; skipping the rest of this run"
                );
                // This project (its publish attempt hit the hub) and every
                // project this run never got to are re-marked dirty for a
                // later trigger, once the app is updated.
                remark_dirty_no_kick(&project_id);
                remaining.for_each(|id| remark_dirty_no_kick(&id));
                return;
            }
            Err(error) => {
                tracing::warn!(project_id = %project_id, error = %error, "auto-publish failed");
            }
        }
    }
}

/// Whether a publish error is the A6 publishing-device refusal
/// (`collab_publishing_device:<name>`).
fn is_publishing_device_refusal(msg: &str) -> bool {
    msg.starts_with(&format!(
        "{}:",
        crate::account::client::COLLAB_PUBLISHING_DEVICE
    ))
}

/// Re-marks a project dirty without waking the worker — the `DIRTY`-only
/// half of [`request_auto_publish`], for [`run_publish_pass`]'s run-wide
/// skip paths (signed out, no Collaboration root, `CollabApiOutdated`): the
/// project needs to run again once the condition clears, but kicking here
/// would just re-run the debounce immediately into the same condition.
fn remark_dirty_no_kick(project_id: &str) {
    if let Ok(mut set) = dirty().lock() {
        set.insert(project_id.to_string());
    }
}

/// The debounce loop's shape, with the kick and the per-tick action injected
/// (production is [`spawn_auto_publish_worker`] with [`kick()`]; tests pass a
/// private `Notify` and a counting closure). Identical structure to
/// `collab_exchange::auto_sync_loop_inner`'s pass loop: waiting on `notified()`
/// again after `run_pass` returns is what makes a request that arrived DURING
/// the run re-arm the loop for exactly one more pass — `Notify::notify_one`
/// stores at most one permit, so however many requests land while nobody is
/// waiting, the next `notified().await` returns immediately exactly once.
async fn auto_publish_loop_inner<F, Fut>(debounce: Duration, kick: &Notify, run_pass: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    loop {
        kick.notified().await;
        tokio::time::sleep(debounce).await;
        run_pass().await;
    }
}

/// Arm the auto-publish worker for this process. NOT independently
/// guarded — the one caller, `api::collab_live::spawn_collab_live`, spawns
/// it once per process behind its own swap-guard.
#[cfg_attr(test, allow(dead_code))] // armed by the live exchange's production spawner only
pub(crate) fn spawn_auto_publish_worker(
    ctx: Arc<ServiceContext>,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Option<tokio::task::JoinHandle<()>> {
    tracing::info!("collab auto-publish worker armed");
    Some(tokio::spawn(auto_publish_loop_inner(
        AUTO_PUBLISH_DEBOUNCE,
        kick(),
        move || {
            let ctx = Arc::clone(&ctx);
            let emitter = emitter.clone();
            async move {
                let due = drain_due_projects(&ctx);
                let publish_ctx = Arc::clone(&ctx);
                run_publish_pass(&ctx, emitter, due, move |project_id, emitter| {
                    let ctx = Arc::clone(&publish_ctx);
                    async move { auto_publish_collab_frames(&ctx, &project_id, emitter).await }
                })
                .await;
            }
        },
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`DIRTY`]/[`DIRTY_SETS`]/[`ALL_DIRTY`] are process-global statics:
    /// tests that mark-then-drain them must not run concurrently, or they
    /// steal each other's entries (same discipline as
    /// `collab_exchange`'s `PKG_CHANGE_DRAIN_LOCK`). The loop-timing tests
    /// below use their own private `Notify` and never touch these statics,
    /// so they need no lock.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        (tmp, ctx)
    }

    fn sample_project(id: &str, auto_publish: bool) -> crate::db::collab::CollabProjectRow {
        crate::db::collab::CollabProjectRow {
            project_id: id.to_string(),
            slug: format!("{id}-slug"),
            title: format!("Project {id}"),
            data_role: "send_receive".into(),
            is_coordinator: true,
            require_approval: false,
            pending_frames: 0,
            project_status: "active".into(),
            target_name: "M101".into(),
            target_ra_deg: 210.8,
            target_dec_deg: 54.35,
            target_radius_deg: 1.5,
            membership_version: 1,
            snapshot_payload_b64: "cGF5bG9hZA==".into(),
            snapshot_signature_b64: "c2ln".into(),
            members_json: "[]".into(),
            thresholds_version: Some(1),
            thresholds_rules_json: Some("[]".into()),
            gov_caps_json: "[]".into(),
            auto_replicate: true,
            synced_caps_json: "[]".into(),
            hub_version: 0,
            manifest_cursor: 0,
            dictionary_version: None,
            dictionary_json: None,
            policy_json: r#"{"mode":"all"}"#.into(),
            replication_paused: false,
            auto_publish,
            fetched_at: String::new(),
            feed_epoch: None,
            holder_seq: -1,
        }
    }

    /// Clears the global dirty state so a test starts from a known-empty
    /// baseline (a leftover mark from another test would only ever resolve
    /// to a project id absent from THIS test's fresh db, but `ALL_DIRTY` is
    /// a single shared bool and must be reset explicitly).
    fn reset_dirty_state() {
        ALL_DIRTY.store(false, Ordering::SeqCst);
        dirty().lock().unwrap().clear();
        dirty_sets().lock().unwrap().clear();
    }

    // ── drain_due_projects ──────────────────────────────────────────────────

    #[test]
    fn sets_map_to_their_linked_projects() {
        let _guard = test_lock();
        reset_dirty_state();
        let (_tmp, ctx) = test_ctx();
        let conn = crate::api::db(&ctx).unwrap().conn();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (42, 'S')", [])
            .unwrap();
        crate::db::collab::upsert_project(&conn, &sample_project("p-set", true)).unwrap();
        crate::db::collab::link_set(&conn, "p-set", 42).unwrap();
        drop(conn);

        request_auto_publish_for_sets(&[42]);
        let due = drain_due_projects(&ctx);
        assert_eq!(due, vec!["p-set".to_string()]);

        // Draining consumes the dirty state — a second drain with nothing new
        // marked finds nothing due.
        assert!(drain_due_projects(&ctx).is_empty());
    }

    #[test]
    fn auto_publish_off_is_skipped() {
        let _guard = test_lock();
        reset_dirty_state();
        let (_tmp, ctx) = test_ctx();
        let conn = crate::api::db(&ctx).unwrap().conn();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (7, 'S')", [])
            .unwrap();
        // `upsert_project` never writes `auto_publish` (it's LOCAL, P13) —
        // `set_auto_publish` is the only writer, same as the real toggle path.
        crate::db::collab::upsert_project(&conn, &sample_project("p-off", true)).unwrap();
        crate::db::collab::set_auto_publish(&conn, "p-off", false).unwrap();
        crate::db::collab::link_set(&conn, "p-off", 7).unwrap();
        drop(conn);

        request_auto_publish(Some("p-off"));
        assert!(
            drain_due_projects(&ctx).is_empty(),
            "auto_publish = 0 is never due"
        );
    }

    /// A project with no linked set is never due either, even with
    /// `auto_publish = 1` — nothing to publish.
    #[test]
    fn unlinked_project_is_skipped() {
        let _guard = test_lock();
        reset_dirty_state();
        let (_tmp, ctx) = test_ctx();
        let conn = crate::api::db(&ctx).unwrap().conn();
        crate::db::collab::upsert_project(&conn, &sample_project("p-bare", true)).unwrap();
        drop(conn);

        request_auto_publish(Some("p-bare"));
        assert!(drain_due_projects(&ctx).is_empty());
    }

    /// The Task 8 controller carry: `on_thresholds_or_dictionary_moved` may
    /// fire twice for one change (a UI refresh and the version poll
    /// overlapping). Both calls mark the SAME project id into the [`DIRTY`]
    /// `HashSet`, so they collapse into one dirty entry — combined with
    /// `KICK`'s single stored permit (proven independently by
    /// `burst_is_coalesced_by_the_debounce`), that's one run.
    #[test]
    fn two_gate_moves_for_the_same_project_drain_to_one_entry() {
        let _guard = test_lock();
        reset_dirty_state();
        let (_tmp, ctx) = test_ctx();
        let conn = crate::api::db(&ctx).unwrap().conn();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'S')", [])
            .unwrap();
        crate::db::collab::upsert_project(&conn, &sample_project("p-thr", true)).unwrap();
        crate::db::collab::link_set(&conn, "p-thr", 1).unwrap();
        drop(conn);

        crate::api::collab::on_thresholds_or_dictionary_moved(&ctx, "p-thr");
        crate::api::collab::on_thresholds_or_dictionary_moved(&ctx, "p-thr");

        assert_eq!(drain_due_projects(&ctx), vec!["p-thr".to_string()]);
    }

    /// R14: `get_project` still reads a lost project (only `list_projects`
    /// filters it) and `project_links` survives a loss — so a lost project
    /// must never come out of `drain_due_projects` via EITHER path: a direct
    /// mark (`DIRTY`, e.g. a stale trigger that raced the loss) or a
    /// set-scoped mark (`DIRTY_SETS` → `project_ids_for_sets`, which reads
    /// `project_links` raw, no `lost_at` join).
    #[test]
    fn a_lost_project_is_never_due_via_either_path() {
        let _guard = test_lock();
        reset_dirty_state();
        let (_tmp, ctx) = test_ctx();
        let conn = crate::api::db(&ctx).unwrap().conn();
        conn.execute("INSERT INTO frames_set (id, name) VALUES (99, 'S')", [])
            .unwrap();
        crate::db::collab::upsert_project(&conn, &sample_project("p-lost", true)).unwrap();
        crate::db::collab::link_set(&conn, "p-lost", 99).unwrap();
        assert_eq!(crate::db::collab::mark_lost(&conn, "p-lost").unwrap(), 1);
        drop(conn);

        request_auto_publish(Some("p-lost"));
        request_auto_publish_for_sets(&[99]);
        assert!(
            drain_due_projects(&ctx).is_empty(),
            "a lost project is never due, dirtied directly or via its set"
        );
    }

    // ── run_publish_pass ─────────────────────────────────────────────────────

    /// A per-project failure (not signed-out / no-root / outdated-hub) is
    /// logged and the loop continues to the next project — worker step 6.
    #[tokio::test]
    async fn failure_does_not_stop_the_worker() {
        let (tmp, ctx) = test_ctx();
        crate::api::account::store_token_for_test(&ctx, "test-token").unwrap();
        let root = tmp.path().join("collab-root");
        std::fs::create_dir_all(&root).unwrap();
        crate::api::scan_roots::set_collaboration_dir(
            &ctx,
            root.to_string_lossy().to_string(),
            &crate::api::PathPolicy::AllowAll,
        )
        .await
        .unwrap();

        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        let empty_result = || PublishResult {
            announced: 0,
            updated: 0,
            state: None,
            held_back: Vec::new(),
            unchanged: 0,
        };
        run_publish_pass(
            &ctx,
            None,
            vec!["p-x".to_string(), "p-y".to_string()],
            move |project_id, _emitter| {
                seen2.lock().unwrap().push(project_id.clone());
                async move {
                    if project_id == "p-x" {
                        Err(ApiError::Internal("boom".into()))
                    } else {
                        Ok(empty_result())
                    }
                }
            },
        )
        .await;

        assert_eq!(
            *seen.lock().unwrap(),
            vec!["p-x".to_string(), "p-y".to_string()]
        );
    }

    /// Signed out, no Collaboration root, or `CollabApiOutdated` are
    /// run-wide — one `warn!`, the rest of `due` is skipped without ever
    /// calling `publish` for it (worker step 7).
    #[tokio::test]
    async fn signed_out_skips_the_whole_run() {
        let (_tmp, ctx) = test_ctx();
        // No `store_token_for_test` — this ctx is signed out.
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls2 = Arc::clone(&calls);
        run_publish_pass(
            &ctx,
            None,
            vec!["p-a".to_string(), "p-b".to_string()],
            move |_project_id, _emitter| {
                calls2.fetch_add(1, Ordering::SeqCst);
                async move {
                    Ok(PublishResult {
                        announced: 0,
                        updated: 0,
                        state: None,
                        held_back: Vec::new(),
                        unchanged: 0,
                    })
                }
            },
        )
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 0, "signed out; nothing ran");
    }

    /// A run-wide skip (signed out here) re-marks every drained-but-unprocessed
    /// project dirty — via [`remark_dirty_no_kick`], never
    /// [`request_auto_publish`] — so the fix (sign back in) is picked up on
    /// the next real trigger instead of the project being lost until
    /// something else happens to dirty it again. `remark_dirty_no_kick`
    /// never touches `KICK`, so this cannot itself re-arm the debounce loop
    /// into busy-looping the same condition (pinned structurally, not by a
    /// synchronous `Notify` probe — `Notify` has no non-consuming peek API).
    #[tokio::test]
    async fn signed_out_remarks_the_drained_projects_dirty() {
        let _guard = test_lock();
        reset_dirty_state();
        let (_tmp, ctx) = test_ctx();
        // No `store_token_for_test` — this ctx is signed out.
        run_publish_pass(
            &ctx,
            None,
            vec!["p-remark".to_string()],
            move |_project_id, _emitter| async move {
                Ok(PublishResult {
                    announced: 0,
                    updated: 0,
                    state: None,
                    held_back: Vec::new(),
                    unchanged: 0,
                })
            },
        )
        .await;

        assert!(
            dirty().lock().unwrap().contains("p-remark"),
            "re-marked dirty for the next trigger"
        );
    }

    /// Final review C1 (owner decision 2026-09-24): while a publish run of
    /// the project holds its lock, the worker's real entry point is refused
    /// at once — nothing runs beside it — and the project is marked dirty
    /// again, so it runs after the current run ends.
    #[tokio::test]
    async fn a_busy_project_is_re_dirtied_not_run_in_parallel() {
        let _guard = test_lock();
        reset_dirty_state();
        let (tmp, ctx) = test_ctx();
        crate::api::account::store_token_for_test(&ctx, "test-token").unwrap();
        let root = tmp.path().join("collab-root");
        std::fs::create_dir_all(&root).unwrap();
        crate::api::scan_roots::set_collaboration_dir(
            &ctx,
            root.to_string_lossy().to_string(),
            &crate::api::PathPolicy::AllowAll,
        )
        .await
        .unwrap();

        let lock = crate::api::collab::publish_lock(&ctx, "p-busy").unwrap();
        let held = lock.lock().await;
        let results: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&results);
        let ctx_ref = &ctx;
        run_publish_pass(
            &ctx,
            None,
            vec!["p-busy".to_string()],
            move |project_id, emitter| {
                let seen = Arc::clone(&seen);
                async move {
                    let r = auto_publish_collab_frames(ctx_ref, &project_id, emitter).await;
                    seen.lock().unwrap().push(format!("{r:?}"));
                    r
                }
            },
        )
        .await;
        drop(held);

        let results = results.lock().unwrap().clone();
        assert_eq!(results.len(), 1);
        assert!(
            results[0].contains(PUBLISH_BUSY_MSG),
            "refused, not run: {results:?}"
        );
        assert!(
            dirty().lock().unwrap().contains("p-busy"),
            "re-dirtied so it runs after the current run"
        );
        reset_dirty_state();
    }

    // ── auto_publish_loop_inner ──────────────────────────────────────────────

    /// A burst of five marks that land within the debounce window (here, all
    /// before the loop ever starts waiting) collapses into ONE run —
    /// `Notify::notify_one` stores at most one permit while idle.
    #[tokio::test]
    async fn burst_is_coalesced_by_the_debounce() {
        let notify = Notify::new();
        for _ in 0..5 {
            notify.notify_one();
        }

        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let runs2 = Arc::clone(&runs);
        let task = tokio::spawn(async move {
            auto_publish_loop_inner(Duration::from_millis(20), &notify, move || {
                let runs = Arc::clone(&runs2);
                async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                }
            })
            .await
        });

        wait_until(|| runs.load(Ordering::SeqCst) >= 1, Duration::from_secs(2)).await;
        // Give the loop a chance to have run a stray second pass, if the
        // burst had (wrongly) produced more than one permit.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 1, "five marks, one run");
        task.abort();
    }

    /// Three requests that arrive WHILE a run is in progress re-arm the loop
    /// for exactly one extra run — never zero (they'd be lost) and never
    /// three (they must coalesce, same as a burst before the run started).
    #[tokio::test]
    async fn requests_during_a_run_rearm_once() {
        let notify = Arc::new(Notify::new());
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        // Two independent handles to the SAME underlying `Notify`: one backs
        // the `&Notify` the loop waits on (borrowed for the spawned task's
        // whole lifetime), the other is moved into `run_pass` so it can mark
        // "three requests arrived during this run" from inside it — a single
        // `notify_for_kick` couldn't do both (the loop borrows it for the
        // `.await`, `run_pass` would need to move it).
        let notify_for_kick = Arc::clone(&notify);
        let notify_for_marks = Arc::clone(&notify);
        let runs2 = Arc::clone(&runs);
        let task = tokio::spawn(async move {
            auto_publish_loop_inner(Duration::from_millis(5), &notify_for_kick, move || {
                let runs = Arc::clone(&runs2);
                let notify = Arc::clone(&notify_for_marks);
                async move {
                    let n = runs.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        // Simulate three requests arriving during THIS run.
                        notify.notify_one();
                        notify.notify_one();
                        notify.notify_one();
                    }
                }
            })
            .await
        });

        notify.notify_one(); // start the first run
        wait_until(|| runs.load(Ordering::SeqCst) >= 2, Duration::from_secs(2)).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 2, "exactly one extra run");
        task.abort();
    }

    /// Polls `cond` until it's true or `timeout` elapses (test helper — mirrors
    /// `collab_exchange`'s own `wait_until`, kept local rather than shared
    /// across a `pub(crate)` test-support seam for one function).
    async fn wait_until(mut cond: impl FnMut() -> bool, timeout: Duration) {
        let start = std::time::Instant::now();
        while !cond() {
            if start.elapsed() > timeout {
                panic!("condition never became true within {timeout:?}");
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}
