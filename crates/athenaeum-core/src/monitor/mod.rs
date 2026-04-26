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
        }
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
        let _ = tokio::task::spawn_blocking(move || {
            orchestrator::run_cycle(&ctx, &*emitter, &offline_roots);
        })
        .await;
    }
}
