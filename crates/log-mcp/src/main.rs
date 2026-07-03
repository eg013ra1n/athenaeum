//! `log-mcp`: a stdio MCP server exposing Athenaeum's rotating JSONL
//! application logs (written by `athenaeum_core::logging`) as queryable
//! tools (`query_logs`, `tail_logs`, `list_operations`, `get_operation`).
//!
//! Framing: line-delimited JSON-RPC on stdin/stdout — one JSON object per
//! line, no `Content-Length` header (MCP stdio transport). The log
//! directory is `argv[1]` if given, else resolved the same way
//! `athenaeum-core` resolves it (see `query::default_log_dir`).
//!
//! Zero-print rule: this process's stdout is the JSON-RPC transport
//! itself, so responses go out via `serde_json::to_writer` / `write!`
//! only, never via a bare print macro. Diagnostics that would otherwise
//! go to stderr are instead surfaced as JSON-RPC error responses.

use log_mcp::{query, rpc};
use std::io::{BufRead, Write};
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let log_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .or_else(query::default_log_dir)
        .expect("log dir: pass as argv[1] or have the app-data dir present");

    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: rpc::Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(_) => continue, // malformed line: skip, don't crash
        };
        if let Some(resp) = rpc::handle(&req, &log_dir) {
            serde_json::to_writer(&mut out, &resp)?;
            writeln!(out)?;
            out.flush()?;
        }
    }
    Ok(())
}
