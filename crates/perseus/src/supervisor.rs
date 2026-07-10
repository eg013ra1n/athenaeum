//! The supervisor: a readiness-driven lifecycle around the capture [`Agent`].
//!
//! Perseus is an installable app. Its web status page stays up for the whole
//! process lifetime, but the sync **engine** should only run once the node is
//! actually ready to sync — signed in (a stored hub device token, or a dev
//! pairing ticket) AND at least one capture directory configured. The supervisor
//! owns that decision: it re-reads `perseus.toml`, computes the current
//! [`AgentState`], and launches or stops the agent as readiness comes and goes.
//!
//! The engine is created through a [`Launcher`] seam rather than by calling
//! [`Agent::start`] directly, so tests drive the exact same state machine over a
//! fake agent (no network, no filesystem watchers) while production wires
//! [`production_launcher`]. Task 4 uses the `on_agent` callback to attach and
//! detach the web server's [`WebState`](crate::web::WebState) as the engine
//! comes and goes.

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use athenaeum_core::sync::SyncEngineHandle;
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;

use crate::config::{Config, RetentionConfig};
use crate::run::Agent;
use crate::web::RetentionRunRecord;

/// The observable lifecycle state of the capture node, published on a
/// [`watch`] channel for the web status page (Task 4) and the tray (Task 8).
#[derive(Debug, Clone, PartialEq)]
pub enum AgentState {
    /// Setup is incomplete: `needs` holds the [`SetupNeed`](crate::config::SetupNeed)
    /// display strings still blocking the engine (no capture dirs, not signed in).
    NeedsSetup { needs: Vec<String> },
    /// Ready and mid-launch — the launcher is building the engine.
    Starting,
    /// The engine is running; `in_flight` is the live non-terminal package count.
    Running { in_flight: u32 },
    /// The last launch attempt failed; the supervisor retries after a backoff.
    Failed { error: String },
}

impl AgentState {
    /// A stable snake_case label for logs / the status API.
    pub fn label(&self) -> &'static str {
        match self {
            AgentState::NeedsSetup { .. } => "needs_setup",
            AgentState::Starting => "starting",
            AgentState::Running { .. } => "running",
            AgentState::Failed { .. } => "failed",
        }
    }

    /// Human-readable detail for the state: the joined setup needs, or the error
    /// text. `None` for the transient `Starting` / a `Running` state.
    pub fn detail(&self) -> Option<String> {
        match self {
            AgentState::NeedsSetup { needs } if !needs.is_empty() => Some(needs.join("; ")),
            AgentState::Failed { error } => Some(error.clone()),
            _ => None,
        }
    }
}

/// What the supervisor needs from a running agent. [`Agent`] implements it for
/// production; tests implement a fake. The accessor methods (`engine`,
/// `peer_device`, `retention_tx`, `retention_log`) exist so Task 4 can build the
/// web [`WebState`](crate::web::WebState) from the `on_agent` callback without the
/// supervisor depending on the web layer.
pub trait ManagedAgent: Send + 'static {
    /// The running sync engine handle, if any (always `Some` for a live agent).
    fn engine(&self) -> Option<Arc<SyncEngineHandle>>;
    /// The configured sync peer id (hex).
    fn peer_device(&self) -> String;
    /// The retention live-edit sender (Task 8's web settings page writes here).
    fn retention_tx(&self) -> watch::Sender<RetentionConfig>;
    /// The rolling retention-pass log the status page serves read-only.
    fn retention_log(&self) -> Arc<Mutex<VecDeque<RetentionRunRecord>>>;
    /// The live in-flight (non-terminal) outbound package count.
    fn in_flight(&self) -> anyhow::Result<usize>;
    /// Gracefully stop the agent, returning a handle that completes on shutdown.
    fn stop(self: Box<Self>) -> JoinHandle<()>;
}

impl ManagedAgent for Agent {
    fn engine(&self) -> Option<Arc<SyncEngineHandle>> {
        Some(self.engine_handle())
    }
    fn peer_device(&self) -> String {
        self.peer_device()
    }
    fn retention_tx(&self) -> watch::Sender<RetentionConfig> {
        Agent::retention_tx(self)
    }
    fn retention_log(&self) -> Arc<Mutex<VecDeque<RetentionRunRecord>>> {
        Agent::retention_log(self)
    }
    fn in_flight(&self) -> anyhow::Result<usize> {
        Ok(self.status_snapshot()?.len())
    }
    fn stop(self: Box<Self>) -> JoinHandle<()> {
        tokio::spawn(async move { (*self).shutdown().await })
    }
}

/// The engine-construction seam. Given a [`Config`] and the on-disk config path,
/// build a running agent (or fail). Boxed so the supervisor is transport- and
/// runtime-agnostic; production wires [`production_launcher`], tests a fake.
pub type Launcher = Arc<
    dyn Fn(Config, PathBuf) -> Pin<Box<dyn Future<Output = anyhow::Result<Box<dyn ManagedAgent>>> + Send>>
        + Send
        + Sync,
>;

/// Tunable timings for the supervisor loop. [`Default`] mirrors production:
/// retry a failed launch after 30s, refresh the running in-flight count every 2s.
/// Tests inject small values.
pub struct SupervisorOptions {
    /// How long to stay in `Failed` before retrying a launch, and the idle
    /// re-check cadence while not running.
    pub retry_backoff: Duration,
    /// How often to refresh the in-flight count while the engine is running.
    pub running_tick: Duration,
}

impl Default for SupervisorOptions {
    fn default() -> Self {
        Self {
            retry_backoff: Duration::from_secs(30),
            running_tick: Duration::from_secs(2),
        }
    }
}

/// Handle to a spawned supervisor loop: the live state channel, a wake handle to
/// prod it into re-reading the config immediately (after a config edit), and the
/// private shutdown plumbing consumed by [`shutdown`](Self::shutdown).
pub struct SupervisorHandle {
    /// Live lifecycle state — clone the receiver for each observer.
    pub state: watch::Receiver<AgentState>,
    /// Prod the loop into an immediate config re-read (Task 6 rings this after a
    /// live config edit so a capture-dir change is picked up without waiting for
    /// the next idle tick).
    pub wake: Arc<Notify>,
    shutdown_tx: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl SupervisorHandle {
    /// Stop the running agent gracefully and end the loop, awaiting its exit.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        self.wake.notify_one();
        let _ = self.task.await;
    }
}

/// Spawn the supervisor loop. `on_agent` fires **synchronously** with
/// `Some(&dyn ManagedAgent)` right after every successful launch (before the
/// `Running` state is published) and with `None` right **before** every stop —
/// Task 4 attaches / detaches the web `WebState` here.
///
/// This is the test-facing entry point: it owns the lifecycle channel + wake and
/// runs with a no-op config hook. Production goes through [`start_supervised`],
/// which builds the always-on web page first and then calls [`spawn_with`] with
/// the web-owned wake + state channel and a config-refresh hook.
pub fn spawn(
    config_path: PathBuf,
    launcher: Launcher,
    opts: SupervisorOptions,
    on_agent: Box<dyn Fn(Option<&dyn ManagedAgent>) + Send>,
) -> SupervisorHandle {
    let wake = Arc::new(Notify::new());
    let (state_tx, _state_rx) = watch::channel(AgentState::NeedsSetup { needs: vec![] });
    spawn_with(
        config_path,
        launcher,
        opts,
        on_agent,
        Box::new(|_| {}),
        wake,
        state_tx,
    )
}

/// The full supervisor engine. Unlike [`spawn`], the lifecycle `state_tx` and
/// `wake` are supplied by the caller ([`start_supervised`]) so the always-on web
/// [`WebState`](crate::web::WebState) can hold the matching receiver and ring the
/// wake **before** the loop starts. `on_config` fires once per pass with the
/// freshly-loaded config so the web DTOs track on-disk edits even in setup mode.
fn spawn_with(
    config_path: PathBuf,
    launcher: Launcher,
    opts: SupervisorOptions,
    on_agent: Box<dyn Fn(Option<&dyn ManagedAgent>) + Send>,
    on_config: Box<dyn Fn(&Config) + Send>,
    wake: Arc<Notify>,
    state_tx: watch::Sender<AgentState>,
) -> SupervisorHandle {
    let state_rx = state_tx.subscribe();
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let wake2 = Arc::clone(&wake);
    let task = tokio::spawn(async move {
        let mut agent: Option<Box<dyn ManagedAgent>> = None;
        // The capture-dir set the running agent was launched for; a divergence
        // from the freshly-read config triggers a restart.
        let mut running_dirs: Vec<PathBuf> = vec![];
        // When set and still in the future, the loop stays in `Failed` (no
        // relaunch) until this instant elapses.
        let mut backoff_until: Option<Instant> = None;

        loop {
            let config = match Config::load_lenient(&config_path) {
                Ok(c) => c,
                Err(e) => {
                    // Surface the full error chain (parse detail included) so the
                    // tray status line and web banner name the actual problem.
                    let error = format!("{e:#}");
                    tracing::error!(%error, path = %config_path.display(), "config load failed");
                    let _ = state_tx.send(AgentState::Failed { error });
                    wait_tick(&wake2, &mut shutdown_rx, opts.retry_backoff).await;
                    if *shutdown_rx.borrow() {
                        break;
                    }
                    continue;
                }
            };
            // Refresh the web's view of the config every pass (retention /
            // capture-dirs DTOs track on-disk edits, setup mode included).
            on_config(&config);
            let needs = config.setup_needs(crate::account::token_present(&config));
            let configured = config.capture_dirs_resolved();

            if !needs.is_empty() {
                // Setup is (no longer) complete: stop any running engine and
                // surface the outstanding needs.
                if let Some(a) = agent.take() {
                    tracing::info!("stopping engine (setup no longer complete)");
                    on_agent(None);
                    let _ = a.stop().await;
                    running_dirs.clear();
                }
                let _ = state_tx.send(AgentState::NeedsSetup {
                    needs: needs.iter().map(|n| n.to_string()).collect(),
                });
            } else if agent.is_some() && running_dirs != configured {
                // Ready, but the capture-dir set changed under a running engine:
                // stop it and relaunch on the very next pass (bounded fast path —
                // no wait between the stop and the relaunch).
                tracing::info!("capture dirs changed; restarting engine");
                let a = agent.take().unwrap();
                on_agent(None);
                let _ = a.stop().await;
                running_dirs.clear();
                continue;
            } else if agent.is_none() {
                // Ready and nothing running: launch, unless we're still inside a
                // post-failure backoff window.
                if backoff_until.is_some_and(|t| Instant::now() < t) {
                    // Hold in `Failed` until the backoff elapses.
                } else {
                    let _ = state_tx.send(AgentState::Starting);
                    match launcher(config.clone(), config_path.clone()).await {
                        Ok(a) => {
                            on_agent(Some(a.as_ref()));
                            running_dirs = configured.clone();
                            let n = a.in_flight().unwrap_or(0) as u32;
                            agent = Some(a);
                            backoff_until = None;
                            let _ = state_tx.send(AgentState::Running { in_flight: n });
                            tracing::info!(in_flight = n, "engine running");
                        }
                        Err(e) => {
                            let error = format!("{e:#}");
                            tracing::error!(%error, "engine start failed");
                            backoff_until = Some(Instant::now() + opts.retry_backoff);
                            let _ = state_tx.send(AgentState::Failed { error });
                        }
                    }
                }
            } else if let Some(a) = &agent {
                // Ready and running: refresh the in-flight count.
                match a.in_flight() {
                    Ok(n) => {
                        let n = n as u32;
                        state_tx.send_if_modified(|s| match s {
                            AgentState::Running { in_flight } if *in_flight != n => {
                                *in_flight = n;
                                true
                            }
                            AgentState::Running { .. } => false,
                            other => {
                                *other = AgentState::Running { in_flight: n };
                                true
                            }
                        });
                    }
                    Err(error) => tracing::warn!(%error, "in-flight snapshot failed"),
                }
            }

            let tick = if agent.is_some() {
                opts.running_tick
            } else {
                opts.retry_backoff
            };
            wait_tick(&wake2, &mut shutdown_rx, tick).await;
            if *shutdown_rx.borrow() {
                break;
            }
        }

        // Graceful stop on shutdown.
        if let Some(a) = agent.take() {
            on_agent(None);
            let _ = a.stop().await;
        }
        tracing::info!("supervisor stopped");
    });
    SupervisorHandle {
        state: state_rx,
        wake,
        shutdown_tx,
        task,
    }
}

/// Park the loop until one of: the [`wake`](SupervisorHandle::wake) handle is
/// notified (a config edit), `tick` elapses (the idle / running re-check
/// cadence), or shutdown is requested. The caller re-checks the shutdown flag
/// afterward, so a shutdown that arrives mid-pass is still honored on the next
/// pass boundary.
async fn wait_tick(wake: &Notify, shutdown: &mut watch::Receiver<bool>, tick: Duration) {
    tokio::select! {
        _ = wake.notified() => {}
        _ = tokio::time::sleep(tick) => {}
        _ = shutdown.changed() => {}
    }
}

/// The production launcher: build a real [`Agent`] with the capture watcher armed.
pub fn production_launcher() -> Launcher {
    Arc::new(|config, path| {
        Box::pin(async move {
            let agent = Agent::start(config, path, true).await?;
            Ok(Box::new(agent) as Box<dyn ManagedAgent>)
        })
    })
}

/// Production entry point: bring up the **always-on** web status page, then run
/// the readiness supervisor with that page's [`WebState`](crate::web::WebState)
/// attached / detached as the engine comes and goes.
///
/// The page is bound **once** here (loopback by default) and lives for the whole
/// process, independent of the engine — in setup mode it renders the outstanding
/// `agentState`; once the node is ready and the launcher builds the engine, the
/// `on_agent` seam swaps the live engine bits into the shared `WebState`. The
/// store + seen are opened here (a second WAL connection beside the agent's own)
/// so sent/history read even while detached. An empty `web_bind` skips the bind;
/// a runtime bind failure is non-fatal (logged, swallowed).
///
/// A config that cannot be parsed at startup does NOT abort here: it falls back
/// to [`Config::fallback`] (platform data dir, loopback web page, no token) so
/// the always-on page still binds and shows the error, while the supervisor loop
/// reloads the real file each pass and publishes `Failed { error }` (red tray
/// icon + web banner) until the typo is fixed. The fallback is loopback-only, so
/// the non-loopback-needs-a-token rule is never weakened.
pub async fn start_supervised(config_path: PathBuf) -> Result<SupervisorHandle> {
    use athenaeum_core::sync::store::StandaloneSyncStore;

    use crate::account::PairingCache;
    use crate::seen::SeenStore;
    use crate::web::{build_router, WebState};

    let config = Config::load_lenient(&config_path).unwrap_or_else(|e| {
        tracing::error!(
            error = %format!("{e:#}"),
            path = %config_path.display(),
            "config load failed at startup; serving platform-default web page until the file is fixed"
        );
        Config::fallback()
    });
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("create data dir {}", config.data_dir.display()))?;

    // Web store + seen: a second connection to the same perseus.db beside the
    // agent's own (safe under WAL — the established pattern in this crate), so
    // the page serves sent/history even while the engine is detached (setup).
    let store = Arc::new(
        StandaloneSyncStore::open(config.db_path())
            .with_context(|| format!("open sync store {}", config.db_path().display()))?,
    );
    let seen = Arc::new(
        SeenStore::open(config.db_path())
            .with_context(|| format!("open seen store {}", config.db_path().display()))?,
    );

    // The lifecycle channel + wake are created HERE so the always-on page can
    // hold the receiver (and ring the wake — Task 5's account page prods a
    // re-check after sign-in) before the supervisor loop starts.
    let wake = Arc::new(Notify::new());
    let (state_tx, state_rx) = watch::channel(AgentState::NeedsSetup { needs: vec![] });

    let web_state = Arc::new(WebState::detached(
        Arc::clone(&store),
        Arc::clone(&seen),
        config.clone(),
        config_path.clone(),
        state_rx,
        Arc::clone(&wake),
    ));

    // Bind the always-on status page (loopback default).
    if config.web_bind.is_empty() {
        tracing::info!("web status page disabled (web_bind empty)");
    } else {
        let router = build_router(Arc::clone(&web_state), config.web_token.clone());
        let _ = crate::run::bind_and_spawn_web(&config.web_bind, router).await;
    }

    // ── Engine attach / detach seam ──────────────────────────────────────────
    // `on_agent` is sync; `attach`/`detach` take the write locks (async). So the
    // callback clones the engine-dependent bits out of `&dyn ManagedAgent`
    // synchronously, then `tokio::spawn`s the async swap onto the shared state.
    let data_dir = config.data_dir.clone();
    let attach_config_path = config_path.clone();
    let ws_agent = Arc::clone(&web_state);
    let on_agent: Box<dyn Fn(Option<&dyn ManagedAgent>) + Send> =
        Box::new(move |agent: Option<&dyn ManagedAgent>| {
            let ws = Arc::clone(&ws_agent);
            match agent {
                Some(agent) => {
                    let engine = agent.engine();
                    let peer_device = agent.peer_device();
                    let retention_tx = agent.retention_tx();
                    let retention_log = agent.retention_log();
                    let device_names = PairingCache::load(&data_dir).device_names;
                    // The dirs the engine was launched over — read from the same
                    // config file the launcher just used (authoritative, sync).
                    let running_dirs = Config::load_lenient(&attach_config_path)
                        .map(|c| c.capture_dirs_resolved())
                        .unwrap_or_default();
                    tokio::spawn(async move {
                        ws.attach(
                            engine,
                            peer_device,
                            retention_tx,
                            retention_log,
                            device_names,
                            running_dirs,
                        )
                        .await;
                    });
                }
                None => {
                    tokio::spawn(async move { ws.detach().await });
                }
            }
        });

    // `on_config` refreshes the web view of the config each pass. `try_write`
    // never blocks: a concurrent handler write simply wins this tick and the
    // next pass reconciles.
    let ws_config = Arc::clone(&web_state);
    let on_config: Box<dyn Fn(&Config) + Send> = Box::new(move |config: &Config| {
        if let Ok(mut guard) = ws_config.config.try_write() {
            *guard = config.clone();
        }
    });

    Ok(spawn_with(
        config_path,
        production_launcher(),
        SupervisorOptions::default(),
        on_agent,
        on_config,
        wake,
        state_tx,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // ── Fakes ────────────────────────────────────────────────────────────────

    /// A per-launch record the test inspects: the agent's in-flight counter and a
    /// flag the supervisor sets via `stop`.
    #[derive(Clone)]
    struct AgentRecord {
        in_flight: Arc<AtomicUsize>,
        stopped: Arc<AtomicBool>,
    }

    /// A fake [`ManagedAgent`] with no engine, watchers, or network — just the
    /// in-flight count and a stop flag the state machine drives.
    struct FakeAgent {
        peer: String,
        in_flight: Arc<AtomicUsize>,
        stopped: Arc<AtomicBool>,
    }

    impl ManagedAgent for FakeAgent {
        fn engine(&self) -> Option<Arc<SyncEngineHandle>> {
            None
        }
        fn peer_device(&self) -> String {
            self.peer.clone()
        }
        fn retention_tx(&self) -> watch::Sender<RetentionConfig> {
            watch::channel(RetentionConfig::default()).0
        }
        fn retention_log(&self) -> Arc<Mutex<VecDeque<RetentionRunRecord>>> {
            Arc::new(Mutex::new(VecDeque::new()))
        }
        fn in_flight(&self) -> anyhow::Result<usize> {
            Ok(self.in_flight.load(Ordering::SeqCst))
        }
        fn stop(self: Box<Self>) -> JoinHandle<()> {
            self.stopped.store(true, Ordering::SeqCst);
            tokio::spawn(async {})
        }
    }

    /// One programmed launcher outcome.
    enum Behavior {
        /// Succeed with this in-flight count.
        Launch(u32),
        /// Fail with this error message.
        Fail(String),
    }

    /// Shared state the fake launcher records into so tests can assert call
    /// counts and inspect each created agent.
    struct FakeLauncherState {
        behaviors: Mutex<VecDeque<Behavior>>,
        calls: AtomicUsize,
        created: Mutex<Vec<AgentRecord>>,
        default_in_flight: u32,
    }

    /// Build a fake launcher: each call pops the next `Behavior` (defaulting to a
    /// successful launch once the queue drains). Returns the launcher plus its
    /// shared state for assertions.
    fn fake_launcher(
        behaviors: Vec<Behavior>,
        default_in_flight: u32,
    ) -> (Launcher, Arc<FakeLauncherState>) {
        let state = Arc::new(FakeLauncherState {
            behaviors: Mutex::new(behaviors.into_iter().collect()),
            calls: AtomicUsize::new(0),
            created: Mutex::new(Vec::new()),
            default_in_flight,
        });
        let st = Arc::clone(&state);
        let launcher: Launcher = Arc::new(move |_config, _path| {
            let st = Arc::clone(&st);
            Box::pin(async move {
                // Yield once so the supervisor's `Starting` publish is observable
                // before the launch outcome lands (single-threaded test runtime).
                tokio::task::yield_now().await;
                st.calls.fetch_add(1, Ordering::SeqCst);
                let behavior = st
                    .behaviors
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Behavior::Launch(st.default_in_flight));
                match behavior {
                    Behavior::Fail(msg) => Err(anyhow::anyhow!(msg)),
                    Behavior::Launch(n) => {
                        let rec = AgentRecord {
                            in_flight: Arc::new(AtomicUsize::new(n as usize)),
                            stopped: Arc::new(AtomicBool::new(false)),
                        };
                        st.created.lock().unwrap().push(rec.clone());
                        let agent = FakeAgent {
                            peer: "peer".to_string(),
                            in_flight: Arc::clone(&rec.in_flight),
                            stopped: Arc::clone(&rec.stopped),
                        };
                        Ok(Box::new(agent) as Box<dyn ManagedAgent>)
                    }
                }
            })
        });
        (launcher, state)
    }

    /// An `on_agent` callback that records `is_some()` for each attach/detach.
    #[allow(clippy::type_complexity)]
    fn recording_on_agent() -> (
        Box<dyn Fn(Option<&dyn ManagedAgent>) + Send>,
        Arc<Mutex<Vec<bool>>>,
    ) {
        let log = Arc::new(Mutex::new(Vec::<bool>::new()));
        let l = Arc::clone(&log);
        let cb = Box::new(move |a: Option<&dyn ManagedAgent>| {
            l.lock().unwrap().push(a.is_some());
        });
        (cb, log)
    }

    fn fast_opts() -> SupervisorOptions {
        SupervisorOptions {
            retry_backoff: Duration::from_millis(50),
            running_tick: Duration::from_millis(20),
        }
    }

    /// Atomic config write (temp + rename) so a mid-write read never sees a
    /// truncated TOML — the supervisor re-reads on every pass.
    fn write_config_atomic(path: &Path, text: &str) {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text).unwrap();
        std::fs::rename(&tmp, path).unwrap();
    }

    /// A ready (signed-in via ticket + ≥1 capture dir) config pointing at real
    /// capture directories.
    fn write_ready_config(path: &Path, data_dir: &Path, dirs: &[&Path]) {
        let dirs_toml = dirs
            .iter()
            .map(|d| format!("\"{}\"", d.display()))
            .collect::<Vec<_>>()
            .join(", ");
        let text = format!(
            "data_dir = \"{}\"\nmode = \"auto\"\ncapture_dirs = [{}]\npairing_ticket = \"t\"\n[retention]\npolicy = \"keep_everything\"\ndry_run = true\n",
            data_dir.display(),
            dirs_toml
        );
        write_config_atomic(path, &text);
    }

    const T: Duration = Duration::from_secs(5);

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn not_ready_config_publishes_needs_setup_and_never_launches() {
        let data = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("perseus.toml");
        // Template-like: empty capture dirs, an [account] table with no stored
        // token, and no pairing ticket → both setup needs.
        write_config_atomic(
            &cfg_path,
            &format!(
                "data_dir = \"{}\"\nmode = \"auto\"\ncapture_dirs = []\n[account]\nemail = \"me@example.com\"\n[retention]\npolicy = \"keep_everything\"\ndry_run = true\n",
                data.path().display()
            ),
        );

        let (launcher, lstate) = fake_launcher(vec![], 0);
        let (on_agent, _seen) = recording_on_agent();
        let handle = spawn(cfg_path, launcher, fast_opts(), on_agent);
        let mut state = handle.state.clone();

        tokio::time::timeout(
            T,
            state.wait_for(|s| matches!(s, AgentState::NeedsSetup { needs } if needs.len() == 2)),
        )
        .await
        .expect("state never settled to NeedsSetup with both needs")
        .unwrap();

        match state.borrow().clone() {
            AgentState::NeedsSetup { needs } => {
                assert!(
                    needs.iter().any(|n| n.contains("capture folders")),
                    "must list the capture-dirs need: {needs:?}"
                );
                assert!(
                    needs.iter().any(|n| n.contains("not signed in")),
                    "must list the pairing need: {needs:?}"
                );
            }
            other => panic!("expected NeedsSetup, got {other:?}"),
        }
        assert_eq!(
            lstate.calls.load(Ordering::SeqCst),
            0,
            "must never launch while setup is incomplete"
        );
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn ready_config_launches_and_publishes_running() {
        let data = tempfile::tempdir().unwrap();
        let cap = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("perseus.toml");
        write_ready_config(&cfg_path, data.path(), &[cap.path()]);

        let (launcher, lstate) = fake_launcher(vec![Behavior::Launch(3)], 3);
        let (on_agent, seen) = recording_on_agent();
        let handle = spawn(cfg_path, launcher, fast_opts(), on_agent);
        let mut state = handle.state.clone();

        tokio::time::timeout(T, state.wait_for(|s| s.label() == "starting"))
            .await
            .expect("never entered Starting")
            .unwrap();
        tokio::time::timeout(
            T,
            state.wait_for(|s| matches!(s, AgentState::Running { in_flight: 3 })),
        )
        .await
        .expect("never reached Running{3}")
        .unwrap();

        assert_eq!(lstate.calls.load(Ordering::SeqCst), 1, "launched exactly once");
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[true],
            "on_agent must have seen Some once"
        );
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn config_edit_wakes_and_restarts_on_dir_change() {
        let data = tempfile::tempdir().unwrap();
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("perseus.toml");
        write_ready_config(&cfg_path, data.path(), &[a.path()]);

        let (launcher, lstate) =
            fake_launcher(vec![Behavior::Launch(0), Behavior::Launch(0)], 0);
        let (on_agent, seen) = recording_on_agent();
        let handle = spawn(cfg_path.clone(), launcher, fast_opts(), on_agent);
        let mut state = handle.state.clone();

        tokio::time::timeout(T, state.wait_for(|s| s.label() == "running"))
            .await
            .expect("first launch never reached Running")
            .unwrap();
        assert_eq!(lstate.calls.load(Ordering::SeqCst), 1);

        // Add a second capture dir, then prod the supervisor to re-read.
        write_ready_config(&cfg_path, data.path(), &[a.path(), b.path()]);
        handle.wake.notify_one();

        // Restart completes when the launcher has been called a second time and
        // a fresh Running is published.
        tokio::time::timeout(T, async {
            loop {
                if state.changed().await.is_err() {
                    break;
                }
                if lstate.calls.load(Ordering::SeqCst) >= 2
                    && matches!(&*state.borrow(), AgentState::Running { .. })
                {
                    break;
                }
            }
        })
        .await
        .expect("restart never completed");

        assert_eq!(
            lstate.calls.load(Ordering::SeqCst),
            2,
            "the dir change must trigger a second launch"
        );
        let created = lstate.created.lock().unwrap();
        assert!(
            created[0].stopped.load(Ordering::SeqCst),
            "the old agent must be stopped on a dir change"
        );
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[true, false, true],
            "on_agent: attach, detach (restart), attach"
        );
        drop(created);
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn losing_readiness_stops_agent() {
        let data = tempfile::tempdir().unwrap();
        let cap = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("perseus.toml");
        write_ready_config(&cfg_path, data.path(), &[cap.path()]);

        let (launcher, lstate) = fake_launcher(vec![Behavior::Launch(0)], 0);
        let (on_agent, seen) = recording_on_agent();
        let handle = spawn(cfg_path.clone(), launcher, fast_opts(), on_agent);
        let mut state = handle.state.clone();

        tokio::time::timeout(T, state.wait_for(|s| s.label() == "running"))
            .await
            .expect("first launch never reached Running")
            .unwrap();

        // Remove the pairing route entirely → "not signed in".
        write_config_atomic(
            &cfg_path,
            &format!(
                "data_dir = \"{}\"\nmode = \"auto\"\ncapture_dirs = [\"{}\"]\n[retention]\npolicy = \"keep_everything\"\ndry_run = true\n",
                data.path().display(),
                cap.path().display()
            ),
        );
        handle.wake.notify_one();

        tokio::time::timeout(
            T,
            state.wait_for(|s| {
                matches!(s, AgentState::NeedsSetup { needs }
                    if needs.len() == 1 && needs[0].contains("not signed in"))
            }),
        )
        .await
        .expect("never returned to NeedsSetup")
        .unwrap();

        assert!(
            lstate.created.lock().unwrap()[0]
                .stopped
                .load(Ordering::SeqCst),
            "the agent must be stopped when readiness is lost"
        );
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[true, false],
            "on_agent: attach, then detach on readiness loss"
        );
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn launch_failure_publishes_failed_and_retries_after_backoff() {
        let data = tempfile::tempdir().unwrap();
        let cap = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("perseus.toml");
        write_ready_config(&cfg_path, data.path(), &[cap.path()]);

        let (launcher, lstate) =
            fake_launcher(vec![Behavior::Fail("boom".into()), Behavior::Launch(1)], 1);
        let (on_agent, _seen) = recording_on_agent();
        let handle = spawn(cfg_path, launcher, fast_opts(), on_agent);
        let mut state = handle.state.clone();

        tokio::time::timeout(
            T,
            state.wait_for(|s| matches!(s, AgentState::Failed { error } if error.contains("boom"))),
        )
        .await
        .expect("never published Failed")
        .unwrap();

        tokio::time::timeout(T, state.wait_for(|s| matches!(s, AgentState::Running { .. })))
            .await
            .expect("never retried into Running")
            .unwrap();

        assert_eq!(
            lstate.calls.load(Ordering::SeqCst),
            2,
            "one failed attempt then one successful retry"
        );
        handle.shutdown().await;
    }

    /// A broken (unparseable) config on disk must publish `Failed { error }`
    /// naming the config problem — never launch — and then recover to a normal
    /// state once the file is fixed and the wake is rung. This is the supervisor
    /// half of the "typo'd TOML must not die silently" fix: the loop owns the
    /// recovery, so `start_supervised` can hand it a config path even when the
    /// initial load failed.
    #[tokio::test]
    async fn invalid_config_publishes_failed_and_recovers_after_fix() {
        let data = tempfile::tempdir().unwrap();
        let cap = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("perseus.toml");
        // Unterminated table header — a definite TOML syntax error.
        write_config_atomic(&cfg_path, "[unclosed table\n");

        let (launcher, lstate) = fake_launcher(vec![], 0);
        let (on_agent, _seen) = recording_on_agent();
        let handle = spawn(cfg_path.clone(), launcher, fast_opts(), on_agent);
        let mut state = handle.state.clone();

        // The loop surfaces the parse failure as Failed{error} mentioning config.
        tokio::time::timeout(
            T,
            state.wait_for(|s| {
                matches!(s, AgentState::Failed { error } if error.contains("config"))
            }),
        )
        .await
        .expect("never published Failed for the broken config")
        .unwrap();
        assert_eq!(
            lstate.calls.load(Ordering::SeqCst),
            0,
            "must never launch while the config cannot be parsed"
        );

        // Fix the file and prod the loop → it reloads and leaves the Failed state.
        write_ready_config(&cfg_path, data.path(), &[cap.path()]);
        handle.wake.notify_one();

        tokio::time::timeout(
            T,
            state.wait_for(|s| {
                matches!(
                    s,
                    AgentState::Starting
                        | AgentState::Running { .. }
                        | AgentState::NeedsSetup { .. }
                )
            }),
        )
        .await
        .expect("never recovered after the config was fixed")
        .unwrap();

        handle.shutdown().await;
    }
}
