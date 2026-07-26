//! Perseus binary entry point: clap CLI over the [`perseus`] agent library.
//!
//! Subcommands:
//! - `login` — interactive account sign-in (email → OTP); stores the device
//!   token and registers this node as a capture device (task M1).
//! - `run` — supervisor mode: host the local web status page and drive the
//!   watcher/engine lifecycle until Ctrl-C (the service mode). First run on the
//!   platform config path bootstraps a commented default config.
//! - `status` — print a one-shot human summary of config + in-flight transfers.
//! - `enqueue-backlog <dir>` — enqueue FITS/XISF already on disk before the
//!   watcher was running, then drain and exit.
//! - `tray` (feature `tray` only) — menu-bar status icon backed by the same
//!   supervisor + web page as `run`; also the default action when no subcommand
//!   is given. It owns its own tokio runtime, so `main` is a sync `fn`.
//!
//! stdout is reserved for `status` human output and clap's `--help`/errors (CLI
//! UX). Everything else is `tracing` — rolling JSONL under `<data_dir>/logs`
//! plus a human line on stderr. There are no `println!`/`eprintln!` calls.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use perseus::config::Config;
use perseus::run::{backlog_files, init_logging, Agent};

/// Headless capture-node agent for Athenaeum personal sync.
#[derive(Parser, Debug)]
#[command(name = "perseus", version, about, long_about = None)]
struct Cli {
    /// Path to the TOML config file. When omitted, a legacy `./perseus.toml` is
    /// honored if present, otherwise the platform config path is used.
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Sign in to the account (interactive email + one-time code) and register
    /// this node as a capture device paired to the account's primary.
    Login,
    /// Supervisor mode: host the local web page and drive the watcher/engine
    /// lifecycle (runs until Ctrl-C). Bootstraps a default config on first run.
    Run,
    /// Print a one-shot status summary (config + in-flight transfers).
    Status,
    /// Enqueue FITS/XISF files already present under <dir>, then drain and exit.
    EnqueueBacklog {
        /// Directory to scan for pre-existing capture files.
        dir: PathBuf,
    },
    /// Menu-bar tray mode: a status icon backed by the same supervisor + web page
    /// as `run` (also the default when no subcommand is given). Bootstraps a
    /// default config on first run.
    #[cfg(feature = "tray")]
    Tray,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = perseus::config::resolve_config_path(cli.config.clone());

    // No subcommand → tray mode when built with `--features tray`; a headless
    // build has no default action, so it prints help and exits with clap's
    // usage-error code (2), matching a missing-subcommand error.
    let command = match cli.command {
        Some(command) => command,
        None => {
            #[cfg(feature = "tray")]
            {
                Command::Tray
            }
            #[cfg(not(feature = "tray"))]
            {
                use clap::CommandFactory;
                let _ = Cli::command().print_help();
                std::process::exit(2);
            }
        }
    };

    // First-run bootstrap is for the supervisor-hosting modes (`run`, and `tray`
    // when enabled). `login`/`status`/`enqueue-backlog` on a missing file fail
    // with the friendly "read perseus config" error below rather than auto-creating.
    let bootstrap = matches!(command, Command::Run);
    #[cfg(feature = "tray")]
    let bootstrap = bootstrap || matches!(command, Command::Tray);
    if bootstrap {
        perseus::config::ensure_config_exists(&config_path)?;
    }

    // `run`/`tray` host the supervisor; a broken config there must be recoverable
    // rather than fatal (see the lenient load below). This is the same command
    // set as `bootstrap`.
    let supervisor_mode = bootstrap;

    // A lenient load brings up the data dir + logging + startup banner. For the
    // supervisor-hosting modes (`run`, `tray`) a parse failure is NON-fatal: an
    // installed tray app with a typo'd TOML would otherwise die on an unseen
    // stderr line — no log file, no icon. Instead we log under the platform data
    // dir and hand the path to the supervisor, whose loop reloads from disk and
    // publishes `Failed { error }` (red icon + web banner), recovering once the
    // file is fixed. `login`/`status`/`enqueue-backlog` still fail fast.
    let (config, initial_load_error): (Option<Config>, Option<anyhow::Error>) =
        match Config::load_lenient_for_boot(&config_path) {
            Ok(c) => (Some(c), None),
            Err(e) if supervisor_mode => (None, Some(e)),
            Err(e) => return Err(e),
        };

    if let Some(c) = &config {
        std::fs::create_dir_all(&c.data_dir)
            .with_context(|| format!("create data dir {}", c.data_dir.display()))?;
    }
    // Keep the log guard alive for the whole process (flushes on drop). Logging
    // goes under the config's own data dir when it loaded, else the platform
    // default (only reached in supervisor mode on a load failure).
    let log_dir = config
        .as_ref()
        .map(Config::log_dir)
        .unwrap_or_else(|| perseus::config::platform_data_dir().join("logs"));
    let _log_guard = init_logging(&log_dir)?;
    if let Some(e) = &initial_load_error {
        tracing::error!(
            error = %format!("{e:#}"),
            path = %config_path.display(),
            "initial config load failed; supervisor will surface and retry"
        );
    }

    // The tray owns its own tokio runtime — the tao event loop must run on the
    // main thread, so `main` cannot be `#[tokio::main]`. Every other path enters a
    // fresh multi-thread runtime below.
    #[cfg(feature = "tray")]
    if matches!(command, Command::Tray) {
        return perseus::tray::run_tray(config_path);
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async_main(command, config, config_path))
}

/// Runtime dispatch for every non-tray subcommand. `config` is the lenient load
/// used for the startup banner — `None` only for `run` when the file could not
/// be parsed (the supervisor owns recovery). `enqueue-backlog` re-loads strictly;
/// `login`/`status` failed fast in `main` so they always receive `Some`.
async fn async_main(command: Command, config: Option<Config>, config_path: PathBuf) -> Result<()> {
    match command {
        Command::Login => {
            let config = config.context("login requires a readable config")?;
            perseus::account::login(&config).await
        }
        Command::Run => cmd_run(config, config_path).await,
        Command::Status => {
            let config = config.context("status requires a readable config")?;
            cmd_status(config).await
        }
        Command::EnqueueBacklog { dir } => {
            // Strict re-load: enqueue-backlog needs a fully valid config.
            let config = Config::load(&config_path)?;
            cmd_enqueue_backlog(config, config_path, dir).await
        }
        #[cfg(feature = "tray")]
        Command::Tray => unreachable!("tray mode runs before the tokio runtime is created"),
    }
}

/// `run`: hand off to the supervisor (which binds the web page and drives the
/// watcher/engine lifecycle) and block until Ctrl-C, then shut it down cleanly.
/// The passed `config` is used only for the startup banner — the supervisor
/// reloads from disk itself. `None` means the config could not be parsed at
/// startup; the supervisor still comes up (fallback page) and surfaces the error.
async fn cmd_run(config: Option<Config>, config_path: PathBuf) -> Result<()> {
    match &config {
        Some(config) => tracing::info!(
            capture_dirs = ?config.capture_dirs_resolved(),
            data_dir = %config.data_dir.display(),
            "perseus starting (supervisor mode)"
        ),
        None => tracing::warn!(
            "perseus starting (supervisor mode) with an unreadable config; the supervisor will surface the error"
        ),
    }
    let handle = perseus::supervisor::start_supervised(config_path).await?;
    tokio::signal::ctrl_c()
        .await
        .context("await Ctrl-C")?;
    tracing::info!("shutdown signal received; stopping");
    handle.shutdown().await;
    Ok(())
}

/// `status`: read-only summary to stdout. No engine/transport is started.
async fn cmd_status(config: Config) -> Result<()> {
    use athenaeum_core::sync::store::StandaloneSyncStore;
    use athenaeum_core::sync::SyncStore;

    // Human-facing CLI output on stdout (documented exemption from zero-print).
    println!("Perseus status");
    println!(
        "  capture_dirs      : {}",
        config
            .capture_dirs_resolved()
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("  data_dir          : {}", config.data_dir.display());
    let pairing_route = match &config.account {
        Some(a) => format!("account (hub {})", a.hub_url),
        None if config
            .pairing_ticket
            .as_ref()
            .is_some_and(|t| !t.trim().is_empty()) =>
        {
            "dev pairing ticket".to_string()
        }
        None => "(none configured)".to_string(),
    };
    println!("  pairing_route     : {pairing_route}");
    let targets = if config.targets.is_empty() {
        "(none configured)".to_string()
    } else {
        config.targets.join(", ")
    };
    println!("  targets           : {targets}");
    // Setup readiness derived from capture folders + a usable send target.
    let needs = config.setup_needs(perseus::account::token_present(&config));
    let readiness = if needs.is_empty() {
        "ready".to_string()
    } else {
        let joined = needs
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        format!("needs — {joined}")
    };
    println!("  agent readiness   : {readiness}");
    println!("  mode              : {:?}", config.mode);
    println!("  retention.policy  : {:?}", config.retention.policy);
    println!(
        "  retention.dry_run : {} (dry-run enforced until the M-Perseus-MVP gate)",
        config.retention.dry_run
    );
    println!(
        "  retention.every   : {}s  keep_days={}  disk_max_pct={}",
        config.retention.interval_secs, config.retention.keep_days, config.retention.disk_max_pct
    );

    let db_path = config.db_path();
    if db_path.exists() {
        let store = StandaloneSyncStore::open(&db_path)
            .with_context(|| format!("open sync store {}", db_path.display()))?;
        let rows = store.non_terminal().context("read in-flight transfers")?;
        println!("  in-flight packages: {}", rows.len());
        for r in &rows {
            println!(
                "    #{:<5} {:<12} attempts={} {}",
                r.id,
                format!("{:?}", r.state),
                r.attempts,
                r.package_ref
            );
        }
    } else {
        println!("  in-flight packages: 0 (no store yet at {})", db_path.display());
    }
    Ok(())
}

/// `enqueue-backlog`: enqueue every eligible file under `dir`, then wait until
/// every package terminalizes (Confirmed/Failed) or Ctrl-C, then shut down.
async fn cmd_enqueue_backlog(config: Config, config_path: PathBuf, dir: PathBuf) -> Result<()> {
    let files = backlog_files(&dir)?;
    tracing::info!(count = files.len(), dir = %dir.display(), "backlog scan complete");
    if files.is_empty() {
        // Nothing to do — surface it on stdout for the operator.
        println!("No FITS/XISF files found under {}", dir.display());
        return Ok(());
    }

    // `watch = false` → no web server spawns, so `config_path` is unused here;
    // passed for signature symmetry with the `run` path.
    let agent = Agent::start(config, config_path, false).await?;
    let mut enqueued = 0usize;
    for path in &files {
        match agent.enqueue_file(path).await {
            Ok(_) => enqueued += 1,
            Err(error) => tracing::error!(%error, path = %path.display(), "backlog enqueue failed"),
        }
    }
    tracing::info!(enqueued, total = files.len(), "backlog enqueued; draining");
    println!("Enqueued {enqueued} of {} file(s); draining…", files.len());

    // Drain: wait until no package is in flight, or Ctrl-C to leave the durable
    // rows for a later `run` to resume.
    loop {
        let in_flight = agent.status_snapshot()?.len();
        if in_flight == 0 {
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Ctrl-C during drain; leaving durable rows for the next run");
                break;
            }
        }
    }

    agent.shutdown().await;
    println!("Done.");
    Ok(())
}
