//! Perseus binary entry point: clap CLI over the [`perseus`] agent library.
//!
//! Subcommands:
//! - `run` — watch the capture dir and auto-send new frames (the service mode).
//! - `status` — print a one-shot human summary of config + in-flight transfers.
//! - `enqueue-backlog <dir>` — enqueue FITS/XISF already on disk before the
//!   watcher was running, then drain and exit.
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
    /// Path to the TOML config file.
    #[arg(short, long, default_value = "perseus.toml", global = true)]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Watch the capture directory and auto-send new frames (runs until Ctrl-C).
    Run,
    /// Print a one-shot status summary (config + in-flight transfers).
    Status,
    /// Enqueue FITS/XISF files already present under <dir>, then drain and exit.
    EnqueueBacklog {
        /// Directory to scan for pre-existing capture files.
        dir: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("create data dir {}", config.data_dir.display()))?;
    // Keep the log guard alive for the whole process (flushes on drop).
    let _log_guard = init_logging(&config.log_dir())?;

    match cli.command {
        Command::Run => cmd_run(config).await,
        Command::Status => cmd_status(config).await,
        Command::EnqueueBacklog { dir } => cmd_enqueue_backlog(config, dir).await,
    }
}

/// `run`: arm the watcher + engine and block until Ctrl-C, then shut down cleanly.
async fn cmd_run(config: Config) -> Result<()> {
    tracing::info!(
        capture_dir = %config.capture_dir.display(),
        data_dir = %config.data_dir.display(),
        "perseus starting (auto mode)"
    );
    let agent = Agent::start(config, true).await?;
    tokio::signal::ctrl_c()
        .await
        .context("await Ctrl-C")?;
    tracing::info!("shutdown signal received; stopping");
    agent.shutdown().await;
    Ok(())
}

/// `status`: read-only summary to stdout. No engine/transport is started.
async fn cmd_status(config: Config) -> Result<()> {
    use athenaeum_core::sync::store::StandaloneSyncStore;
    use athenaeum_core::sync::SyncStore;

    // Human-facing CLI output on stdout (documented exemption from zero-print).
    println!("Perseus status");
    println!("  capture_dir       : {}", config.capture_dir.display());
    println!("  data_dir          : {}", config.data_dir.display());
    println!(
        "  pairing_ticket    : {}",
        if config.pairing_ticket.trim().is_empty() {
            "(missing)"
        } else {
            "configured"
        }
    );
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
async fn cmd_enqueue_backlog(config: Config, dir: PathBuf) -> Result<()> {
    let files = backlog_files(&dir)?;
    tracing::info!(count = files.len(), dir = %dir.display(), "backlog scan complete");
    if files.is_empty() {
        // Nothing to do — surface it on stdout for the operator.
        println!("No FITS/XISF files found under {}", dir.display());
        return Ok(());
    }

    let agent = Agent::start(config, false).await?;
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
