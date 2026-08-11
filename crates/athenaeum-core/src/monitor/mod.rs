//! Background folder monitoring service.
//!
//! Periodically polls monitor-enabled scan roots by reusing the existing
//! `scanner::run_registered_scan` helper. On each cycle, emits a
//! `monitor-detected` event for every root that found new files, so the
//! frontend can surface toasts + populate the notification bell.
//!
//! Design notes:
//! - Polling, not filesystem watchers — see the auto-scanning design spec
//!   (docs: 2026-04-23). Watchers are unreliable on NAS/SMB/NFS where most
//!   astrophotography data lives.
//! - The scanner is idempotent (deduplicates by path), so re-running it on
//!   unchanged folders is effectively free.
//! - Concurrency: a tick skips any root that is already in `active_scans`.
//!   No queuing — the next tick will try again.

pub mod orchestrator;

use crate::events::ProgressEmitter;
use crate::services::ServiceContext;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

/// Host-installed hook invoked after a MONITOR-triggered registered scan
/// completes with newly-ingested files.
///
/// **Monitor-triggered only.** `orchestrator::run_cycle` is the sole call site
/// (it passes the scan's `new_file_ids`), and it is reached only from
/// [`MonitorService::tick`] — an interactive scan does NOT fire this hook. A
/// host that also wants to react to human-triggered scans must wire that at its
/// own command/route boundary; assuming otherwise is how a double-fire gets
/// written.
///
/// Kept as a trait — rather than a concrete host type — so core never depends on
/// `AppState`/Tauri/Axum: a host can implement it once at startup, closing over
/// its own state, and install it via [`MonitorService::set_scan_completion_hook`].
/// A `None` hook (unit tests, a host that never wires one up) must never block or
/// panic a cycle.
///
/// Fire-and-forget by contract: `on_scan_completed` runs synchronously on the
/// monitor's blocking-pool thread ([`MonitorService::tick`]) and must not
/// block on I/O — an async implementation spawns its own task and returns
/// immediately.
pub trait ScanCompletionHook: Send + Sync {
    fn on_scan_completed(&self, new_file_ids: Vec<i64>);
}

/// Configuration for the monitor service, read from DB settings at start.
#[derive(Debug, Clone)]
pub struct MonitorConfig {
    pub interval: Duration,
    pub enabled_global: bool,
}

impl MonitorConfig {
    /// Load config from the settings store, applying defaults on parse errors.
    pub fn from_settings(ctx: &ServiceContext) -> Self {
        use crate::settings::{defaults, keys};

        let (interval_minutes, enabled_global) = if let Some(db) = ctx.db.get() {
            let conn = db.conn();
            let interval_str = ctx
                .settings
                .get_with_precedence(
                    &conn,
                    keys::MONITORING_INTERVAL_MINUTES,
                    defaults::MONITORING_INTERVAL_MINUTES,
                )
                .unwrap_or_else(|_| defaults::MONITORING_INTERVAL_MINUTES.to_string());
            let enabled_str = ctx
                .settings
                .get_with_precedence(
                    &conn,
                    keys::MONITORING_ENABLED_GLOBAL,
                    defaults::MONITORING_ENABLED_GLOBAL,
                )
                .unwrap_or_else(|_| defaults::MONITORING_ENABLED_GLOBAL.to_string());
            let minutes: u64 = interval_str.parse().unwrap_or(10);
            let enabled: bool = enabled_str == "true";
            (minutes, enabled)
        } else {
            (10, true)
        };

        let minutes = interval_minutes.max(1);
        Self {
            interval: Duration::from_secs(minutes * 60),
            enabled_global,
        }
    }
}

/// Handle to a running monitor service. Clone to share; shutdown is broadcast.
#[derive(Clone)]
pub struct MonitorService {
    shutdown: Arc<Notify>,
    /// Wake-up signal. Stores a permit if no one is currently waiting, so a
    /// `kick()` issued during a tick is honored on the very next iteration
    /// instead of being lost.
    kick: Arc<Notify>,
    /// Set of scan_root IDs currently known to be offline. Used to log
    /// availability transitions exactly once per outage instead of once
    /// per tick.
    offline_roots: Arc<Mutex<HashSet<i64>>>,
    /// Optional host-installed post-scan hook (task M2). `None` until a host
    /// calls [`set_scan_completion_hook`](Self::set_scan_completion_hook) — a
    /// bare `MonitorService` (unit tests, or a host that never wires one up)
    /// simply never fires it.
    scan_completion_hook: Arc<Mutex<Option<Arc<dyn ScanCompletionHook>>>>,
}

impl Default for MonitorService {
    fn default() -> Self {
        Self::new()
    }
}

impl MonitorService {
    pub fn new() -> Self {
        Self {
            shutdown: Arc::new(Notify::new()),
            kick: Arc::new(Notify::new()),
            offline_roots: Arc::new(Mutex::new(HashSet::new())),
            scan_completion_hook: Arc::new(Mutex::new(None)),
        }
    }

    /// Install the post-scan hook (task M2). Both hosts call this once at
    /// startup, from the point where the database is guaranteed ready rather
    /// than from where they spawn `run_loop` — desktop inside
    /// `commands::core::initialize_database`, web in `main.rs` after the same
    /// initialisation — closing over their own `ServiceContext` plus whatever
    /// their transport needs to build an emitter per fire (an `AppHandle` on
    /// desktop, an SSE `broadcast::Sender` on web). There is only ever one hook
    /// per process; a second call overwrites the first.
    pub fn set_scan_completion_hook(&self, hook: Arc<dyn ScanCompletionHook>) {
        *self.scan_completion_hook.lock().unwrap() = Some(hook);
    }

    /// The currently-installed hook, if any.
    fn scan_completion_hook(&self) -> Option<Arc<dyn ScanCompletionHook>> {
        self.scan_completion_hook.lock().unwrap().clone()
    }

    /// Trigger a graceful shutdown of any running `run_loop`.
    pub fn shutdown(&self) {
        self.shutdown.notify_waiters();
    }

    /// Wake the polling loop immediately, bypassing the rest of the current
    /// sleep. Used by command handlers that change monitor-relevant state
    /// (e.g. flipping a scan root's `monitor_enabled` to true, or changing
    /// the global interval) so the user sees an immediate scan instead of
    /// waiting up to one full interval.
    pub fn kick(&self) {
        self.kick.notify_one();
    }

    /// Run the polling loop. Intended to be spawned from the backend's tokio
    /// runtime. Exits cleanly when `shutdown()` is called.
    ///
    /// `initial_delay`: wait this long before the first tick (e.g. 3s on
    /// Tauri startup so the UI renders first). Pass `Duration::ZERO` on web.
    pub async fn run_loop<E>(
        self,
        ctx: Arc<ServiceContext>,
        emitter: Arc<E>,
        initial_delay: Duration,
    ) where
        E: ProgressEmitter + 'static,
    {
        // Initial deferred tick (catch-up after app launch).
        tokio::select! {
            _ = self.shutdown.notified() => return,
            _ = tokio::time::sleep(initial_delay) => {}
        }

        // One immediate catch-up pass.
        self.tick(Arc::clone(&ctx), Arc::clone(&emitter)).await;

        // Enter the interval loop.
        loop {
            let config = MonitorConfig::from_settings(&ctx);
            if !config.enabled_global {
                // Poll the global switch again in 30s (cheap), but wake
                // immediately if the user re-enables monitoring.
                tokio::select! {
                    _ = self.shutdown.notified() => return,
                    _ = self.kick.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                }
                continue;
            }

            // Sleep until the interval elapses OR a kick wakes us.
            tokio::select! {
                _ = self.shutdown.notified() => return,
                _ = self.kick.notified() => {}
                _ = tokio::time::sleep(config.interval) => {}
            }

            self.tick(Arc::clone(&ctx), Arc::clone(&emitter)).await;
        }
    }

    /// Run a single polling cycle on a blocking thread pool.
    /// Scanner work is CPU-bound and not itself async, so we hand it off.
    async fn tick<E: ProgressEmitter + 'static>(&self, ctx: Arc<ServiceContext>, emitter: Arc<E>) {
        let offline_roots = Arc::clone(&self.offline_roots);
        let hook = self.scan_completion_hook();
        if let Err(e) = tokio::task::spawn_blocking(move || {
            orchestrator::run_cycle(&ctx, &*emitter, &offline_roots, hook.as_deref());
        })
        .await
        {
            tracing::error!(error = %e, "monitor cycle task panicked or was cancelled");
        }
    }
}
