//! Reads Athenaeum's rotating JSONL application logs (written by
//! `athenaeum_core::logging` via `tracing-subscriber`'s json layer) and
//! answers the four MCP tools' queries.
//!
//! # As-built JSONL envelope (verified against the real `tracing-subscriber
//! 0.3` json layer + `FmtSpan::CLOSE`, not just the crate docs):
//!
//! ```json
//! {"timestamp":"...","level":"INFO","target":"athenaeum_core::scanner",
//!  "fields":{"message":"...", "<event field>":...},
//!  "span":{"root_id":1,"name":"scan"},
//!  "spans":[{"root_id":1,"name":"scan"}]}
//! ```
//!
//! `level` is UPPERCASE. `span`/`spans` are absent entirely for events
//! outside any span. On a span-**close** event, `fields.message == "close"`
//! and carries `time.busy`/`time.idle` (string durations like `"1.23ms"`);
//! critically, `spans` is already `[]` by the time the close event fires
//! (the span has popped off the stack) — the closing span's own fields
//! only survive in `span`. There is no separate `duration_ms`/`outcome`
//! field; a failed operation shows up as an `ERROR`-level event *inside*
//! the span (from `#[instrument(err)]`), not as an attribute on the close
//! event.
//!
//! Operation spans (for `list_operations`/`get_operation`): `scan`
//! (`root_id`), `archive_op` (`operation_id`), `file_op` (`operation_id`),
//! `solve` (`frame_id`), `export` (`frame_set_id`).

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// `(span name, id-field name)` for every operation kind we track.
pub const OPERATION_KINDS: &[(&str, &str)] = &[
    ("scan", "root_id"),
    ("archive_op", "operation_id"),
    ("file_op", "operation_id"),
    ("solve", "frame_id"),
    ("export", "frame_set_id"),
];

pub const DEFAULT_LIMIT: usize = 200;
pub const MAX_LIMIT: usize = 1000;

/// Clamp a user-supplied limit into `[1, MAX_LIMIT]`, defaulting to
/// `DEFAULT_LIMIT` when absent.
pub fn clamp_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub level: Option<String>,
    pub module: Option<String>,
    pub contains: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub operation_id: Option<String>,
    pub limit: usize,
}

/// Severity rank, higher = more severe. `None` for an unrecognized level
/// string (filters fail open rather than silently matching nothing).
fn severity(level: &str) -> Option<i32> {
    match level.to_ascii_uppercase().as_str() {
        "ERROR" => Some(4),
        "WARN" => Some(3),
        "INFO" => Some(2),
        "DEBUG" => Some(1),
        "TRACE" => Some(0),
        _ => None,
    }
}

/// `*.jsonl` files in `dir`, sorted by filename — chronological because
/// `tracing-appender`'s daily rolling prefix format sorts that way
/// (`<prefix>.<date>.jsonl`).
fn log_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .collect();
    files.sort();
    Ok(files)
}

/// Does this span object (an entry from `spans[]`, or the `span` object)
/// carry `id` under any of the known operation id-field names?
fn span_has_id(span: &Value, id: &str) -> bool {
    OPERATION_KINDS.iter().any(|(_, field)| {
        span.get(*field)
            .map(|v| value_matches_str(v, id))
            .unwrap_or(false)
    })
}

fn value_matches_str(v: &Value, id: &str) -> bool {
    match v {
        Value::String(s) => s == id,
        Value::Number(n) => n.to_string() == id,
        _ => false,
    }
}

fn event_matches(value: &Value, f: &Filter) -> bool {
    if let Some(level) = &f.level {
        if let Some(want) = severity(level) {
            let got = value
                .get("level")
                .and_then(|v| v.as_str())
                .and_then(severity)
                .unwrap_or(-1);
            if got < want {
                return false;
            }
        }
    }
    if let Some(module) = &f.module {
        let target = value.get("target").and_then(|v| v.as_str()).unwrap_or("");
        if !target.starts_with(module.as_str()) {
            return false;
        }
    }
    if let Some(needle) = &f.contains {
        // Search the whole serialized event, not just fields.message — a
        // grep-style "contains" is more useful when the match lives in an
        // event field (e.g. an error string) rather than the message text.
        let haystack = value.to_string();
        if !haystack.contains(needle.as_str()) {
            return false;
        }
    }
    if let Some(since) = &f.since {
        let ts = value
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if ts < since.as_str() {
            return false;
        }
    }
    if let Some(until) = &f.until {
        let ts = value
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if ts > until.as_str() {
            return false;
        }
    }
    if let Some(op) = &f.operation_id {
        let in_span = value
            .get("span")
            .map(|s| span_has_id(s, op))
            .unwrap_or(false);
        let in_spans = value
            .get("spans")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().any(|s| span_has_id(s, op)))
            .unwrap_or(false);
        if !in_span && !in_spans {
            return false;
        }
    }
    true
}

/// Stream every `*.jsonl` file in `dir` in chronological order, parsing
/// each line lazily (malformed lines are skipped, never fatal), and keep
/// the last `f.limit` matches.
pub fn scan(dir: &Path, f: &Filter) -> Result<Vec<Value>> {
    scan_with(dir, f.limit, |v| event_matches(v, f))
}

/// Same file-scanning/ring-buffer machinery as `scan`, but with an
/// arbitrary predicate — used by `list_operations`, whose "close event
/// whose span name is an operation kind" match isn't expressible as a
/// plain `Filter`.
fn scan_with(
    dir: &Path,
    limit: usize,
    mut predicate: impl FnMut(&Value) -> bool,
) -> Result<Vec<Value>> {
    let limit = limit.max(1);
    let mut ring: VecDeque<Value> = VecDeque::with_capacity(limit.min(4096));
    for path in log_files(dir)? {
        let content = std::fs::read_to_string(&path)?;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let value: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue, // malformed line: skip, don't crash
            };
            if !predicate(&value) {
                continue;
            }
            if ring.len() == limit {
                ring.pop_front();
            }
            ring.push_back(value);
        }
    }
    Ok(ring.into_iter().collect())
}

/// `tail_logs(n)`: unfiltered scan, last `n` lines.
pub fn tail(dir: &Path, n: usize) -> Result<Vec<Value>> {
    scan(
        dir,
        &Filter {
            limit: n.max(1),
            ..Default::default()
        },
    )
}

/// `get_operation(id)`: every event whose current span stack carries `id`
/// under any operation id-field.
pub fn get_operation(dir: &Path, id: &str, limit: usize) -> Result<Vec<Value>> {
    scan(
        dir,
        &Filter {
            operation_id: Some(id.to_string()),
            limit: limit.max(1),
            ..Default::default()
        },
    )
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OperationSummary {
    pub kind: String,
    pub id: String,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
}

/// `list_operations(kind?, since?)`: span-close events whose span name is
/// one of `OPERATION_KINDS`, projected to `{kind, id, timestamp,
/// duration}`. `duration` comes from `fields."time.busy"` (a single JSON
/// key containing a literal dot, not a nested object).
pub fn list_operations(
    dir: &Path,
    kind: Option<&str>,
    since: Option<&str>,
    limit: usize,
) -> Result<Vec<OperationSummary>> {
    let raw = scan_with(dir, limit, |v| {
        if v.pointer("/fields/message").and_then(|m| m.as_str()) != Some("close") {
            return false;
        }
        let span_name = v
            .pointer("/span/name")
            .and_then(|n| n.as_str())
            .unwrap_or("");
        if !OPERATION_KINDS.iter().any(|(k, _)| *k == span_name) {
            return false;
        }
        if let Some(k) = kind {
            if span_name != k {
                return false;
            }
        }
        if let Some(s) = since {
            let ts = v.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
            if ts < s {
                return false;
            }
        }
        true
    })?;
    Ok(raw.iter().filter_map(project_operation).collect())
}

fn project_operation(v: &Value) -> Option<OperationSummary> {
    let span = v.get("span")?;
    let name = span.get("name").and_then(|n| n.as_str())?.to_string();
    let (_, id_field) = OPERATION_KINDS.iter().find(|(k, _)| *k == name)?;
    let id = match span.get(*id_field)? {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let timestamp = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let duration = v
        .pointer("/fields/time.busy")
        .and_then(|d| d.as_str())
        .map(|s| s.to_string());
    Some(OperationSummary {
        kind: name,
        id,
        timestamp,
        duration,
    })
}

/// Resolve the app-data directory from environment variables (no Tauri
/// needed). Mirrors Tauri's default `app_data_dir` resolution per
/// platform — duplicated (not imported) from
/// `athenaeum_core::logging::resolve_app_data_dir` so this crate stays
/// dependency-free of `athenaeum-core`.
fn resolve_app_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        return std::env::var_os("APPDATA")
            .map(|d| PathBuf::from(d).join("com.vsharifov.athenaeum"));
    }
    #[cfg(target_os = "macos")]
    {
        return std::env::var_os("HOME")
            .map(|d| PathBuf::from(d).join("Library/Application Support/com.vsharifov.athenaeum"));
    }
    #[cfg(target_os = "linux")]
    {
        return std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .map(|d| d.join("com.vsharifov.athenaeum"));
    }
    #[allow(unreachable_code)]
    None
}

/// Default log dir, mirroring `athenaeum_core::logging::resolve_log_dir`'s
/// precedence: `ATHENAEUM_LOG_DIR` > `ATHENAEUM_DB_PATH`'s parent + `logs/`
/// > platform app-data dir + `logs/`.
pub fn default_log_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("ATHENAEUM_LOG_DIR") {
        return Some(PathBuf::from(dir));
    }
    if let Ok(db_path) = std::env::var("ATHENAEUM_DB_PATH") {
        let parent = PathBuf::from(&db_path)
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        return Some(parent.join("logs"));
    }
    resolve_app_data_dir().map(|d| d.join("logs"))
}
