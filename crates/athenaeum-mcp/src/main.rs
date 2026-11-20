//! The program a client starts: `athenaeum-mcp --build release|dev [--app-data-dir <dir>]`. stdin/stdout are the MCP
//! protocol; the log goes to stderr (`ATHENAEUM_MCP_LOG` sets the filter, `info` by default) and never carries the key.

use athenaeum_mcp::{parse_args, run_bridge};
use tracing_subscriber::EnvFilter;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let filter = EnvFilter::try_from_env("ATHENAEUM_MCP_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).with_ansi(false).init();
    let opts = match parse_args(std::env::args().skip(1)) {
        Ok(opts) => opts,
        Err(message) => {
            tracing::error!(error = %message, "the bridge was started with wrong arguments");
            return std::process::ExitCode::from(2);
        }
    };
    tracing::info!(build = opts.build.server_name(), "bridge started");
    match run_bridge(opts, tokio::io::stdin(), tokio::io::stdout()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "the bridge stopped");
            std::process::ExitCode::FAILURE
        }
    }
}
